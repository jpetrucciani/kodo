//! CurseForge through the official API with the user's own key (kodo ships none).
//!
//! The flavor → game version type mapping is resolved at runtime from
//! `/v1/games/1/version-types` by name and is unverified for Forever; when nothing matches,
//! results are not filtered. Fingerprint matching (`POST /v1/fingerprints`) is deferred, so
//! `match_installed` keeps the trait's empty default.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use kodo_core::addon::{AddonRef, Channel, Query, Release, RemoteAddon, Sort, SourceId};
use kodo_core::flavor::Flavor;
use kodo_core::source::{Progress, Source, SourceError};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::http::Http;

pub const API: &str = "https://api.curseforge.com";
const WOW_GAME_ID: u32 = 1;
const KEY_ENV: &str = "KODO_CURSEFORGE_KEY";

#[derive(Deserialize)]
struct Data<T> {
    data: T,
}

#[derive(Debug, Clone, Deserialize)]
struct VersionType {
    id: u64,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Mod {
    id: u64,
    name: String,
    summary: Option<String>,
    download_count: Option<f64>,
    #[serde(default)]
    authors: Vec<Author>,
    links: Option<Links>,
    date_modified: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Author {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Links {
    website_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    display_name: String,
    release_type: u8,
    file_date: String,
    file_length: Option<u64>,
    download_url: Option<String>,
}

pub struct CurseForge {
    http: Arc<Http>,
    api: String,
    key: Option<String>,
    version_types: Mutex<Option<Arc<Vec<VersionType>>>>,
}

impl CurseForge {
    pub fn new(http: Arc<Http>, api: impl Into<String>, key: Option<String>) -> Self {
        Self {
            http,
            api: api.into().trim_end_matches('/').to_owned(),
            key: key.filter(|k| !k.trim().is_empty()),
            version_types: Mutex::new(None),
        }
    }

    fn headers(&self) -> Result<HeaderMap, SourceError> {
        let missing = || SourceError::MissingKey {
            source_id: SourceId::Curseforge,
            env: KEY_ENV,
        };
        let key = self.key.as_deref().ok_or_else(missing)?;
        let mut h = HeaderMap::new();
        h.insert(
            "x-api-key",
            HeaderValue::from_str(key).map_err(|_| missing())?,
        );
        h.insert("accept", HeaderValue::from_static("application/json"));
        Ok(h)
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        addon: Option<&AddonRef>,
    ) -> Result<T, SourceError> {
        let resp: Data<T> = self
            .http
            .get_json(SourceId::Curseforge, url, self.headers()?, false)
            .await
            .map_err(|e| match (e, addon) {
                (SourceError::Http { status: 404, .. }, Some(a)) => {
                    SourceError::NotFound(a.to_string())
                }
                (e, _) => e,
            })?;
        Ok(resp.data)
    }

    /// The game version type id for a flavor, if CurseForge lists one we recognize.
    async fn version_type(&self, flavor: &Flavor) -> Result<Option<u64>, SourceError> {
        let mut guard = self.version_types.lock().await;
        let types = match guard.as_ref() {
            Some(t) => t.clone(),
            None => {
                let url = format!("{}/v1/games/{WOW_GAME_ID}/version-types", self.api);
                let t: Arc<Vec<VersionType>> = Arc::new(self.get(&url, None).await?);
                *guard = Some(t.clone());
                t
            }
        };
        Ok(types
            .iter()
            .find(|t| version_type_matches(flavor, &t.name))
            .map(|t| t.id))
    }

    fn url(&self, path: &str, params: &[(&str, String)]) -> Result<String, SourceError> {
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.api)).map_err(|e| {
            SourceError::Parse {
                url: self.api.clone(),
                message: e.to_string(),
            }
        })?;
        url.query_pairs_mut().extend_pairs(params);
        Ok(url.into())
    }
}

fn version_type_matches(flavor: &Flavor, name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    match flavor {
        Flavor::Forever => n.contains("forever"),
        Flavor::Retail => n.contains("retail"),
        Flavor::ClassicEra => n.trim() == "wow classic" || n.contains("classic era"),
        Flavor::Classic => n.contains("mists"),
        Flavor::Other(_) => false,
    }
}

fn channel_of(release_type: u8) -> Channel {
    match release_type {
        1 => Channel::Stable,
        2 => Channel::Beta,
        _ => Channel::Alpha,
    }
}

fn project_id(addon: &AddonRef) -> Result<&str, SourceError> {
    if addon.source == SourceId::Curseforge {
        Ok(&addon.id)
    } else {
        Err(SourceError::NotFound(addon.to_string()))
    }
}

#[async_trait]
impl Source for CurseForge {
    fn id(&self) -> SourceId {
        SourceId::Curseforge
    }

    async fn search(&self, q: &Query, flavor: &Flavor) -> Result<Vec<RemoteAddon>, SourceError> {
        self.headers()?;
        let limit = if q.limit == 0 { 25 } else { q.limit.min(50) };
        // CurseForge ModsSearchSortField: 2 popularity, 3 last updated, 4 name, 6 downloads.
        let (field, order) = match q.sort {
            Sort::Relevance => ("2", "desc"),
            Sort::Downloads => ("6", "desc"),
            Sort::Updated => ("3", "desc"),
            Sort::Name => ("4", "asc"),
        };
        let mut params = vec![
            ("gameId", WOW_GAME_ID.to_string()),
            ("searchFilter", q.text.trim().to_owned()),
            ("pageSize", limit.to_string()),
            ("sortField", field.to_owned()),
            ("sortOrder", order.to_owned()),
        ];
        if let Some(vt) = self.version_type(flavor).await? {
            params.push(("gameVersionTypeId", vt.to_string()));
        }
        let url = self.url("/v1/mods/search", &params)?;
        let mods: Vec<Mod> = self.get(&url, None).await?;
        Ok(mods
            .into_iter()
            .map(|m| RemoteAddon {
                addon: AddonRef::new(SourceId::Curseforge, m.id.to_string()),
                name: m.name,
                author: m.authors.into_iter().next().map(|a| a.name),
                summary: m.summary,
                downloads: m.download_count.map(|d| d.max(0.0) as u64),
                page_url: m.links.and_then(|l| l.website_url),
                version: None,
                updated: m.date_modified.and_then(|d| d.get(..10).map(str::to_owned)),
                tracks: Vec::new(),
            })
            .collect())
    }

    async fn resolve(
        &self,
        addon: &AddonRef,
        flavor: &Flavor,
        channel: Channel,
    ) -> Result<Release, SourceError> {
        let id = project_id(addon)?;
        self.headers()?;
        let project: Mod = self
            .get(&format!("{}/v1/mods/{id}", self.api), Some(addon))
            .await?;
        let mut params = vec![("pageSize", "50".to_owned())];
        if let Some(vt) = self.version_type(flavor).await? {
            params.push(("gameVersionTypeId", vt.to_string()));
        }
        let url = self.url(&format!("/v1/mods/{id}/files"), &params)?;
        let mut files: Vec<File> = self.get(&url, Some(addon)).await?;
        files.sort_by(|a, b| b.file_date.cmp(&a.file_date));
        let file = files
            .into_iter()
            .find(|f| channel.accepts(channel_of(f.release_type)))
            .ok_or_else(|| SourceError::NoRelease(addon.clone()))?;
        Ok(Release {
            addon: addon.clone(),
            name: project.name,
            version: file.display_name,
            channel: channel_of(file.release_type),
            // `null` when the author disallows third-party distribution.
            download_url: file.download_url,
            page_url: project.links.and_then(|l| l.website_url),
            size: file.file_length,
            changelog: None,
            published_at: Some(file.file_date),
        })
    }

    async fn download(
        &self,
        url: &str,
        dest: &Path,
        progress: Option<Progress>,
    ) -> Result<u64, SourceError> {
        self.http
            .download(SourceId::Curseforge, url, dest, progress)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kodo_core::addon::{AddonRef, Channel, Query, SourceId};
    use kodo_core::flavor::Flavor;
    use kodo_core::source::{Source, SourceError};
    use wiremock::matchers::{header, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::curseforge::CurseForge;
    use crate::http::Http;

    #[tokio::test]
    async fn missing_key_is_reported_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let http = Arc::new(Http::new(dir.path()).unwrap());
        let cf = CurseForge::new(http, "http://127.0.0.1:9", Some("  ".into()));
        let q = Query {
            text: "bags".into(),
            limit: 5,
            ..Query::default()
        };
        let err = cf.search(&q, &Flavor::Forever).await.unwrap_err();
        assert!(matches!(
            err,
            SourceError::MissingKey {
                source_id: SourceId::Curseforge,
                env: "KODO_CURSEFORGE_KEY"
            }
        ));
    }

    #[tokio::test]
    async fn file_without_download_url_points_at_project_page() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let ok = |body: serde_json::Value| ResponseTemplate::new(200).set_body_json(body);
        Mock::given(path("/v1/games/1/version-types"))
            .and(header("x-api-key", "k"))
            .respond_with(ok(serde_json::json!({"data": [
                {"id": 517, "gameId": 1, "name": "WoW Retail", "slug": "wow_retail"},
                {"id": 99001, "gameId": 1, "name": "WoW Forever", "slug": "wow-forever"}
            ]})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/v1/mods/942432"))
            .respond_with(ok(serde_json::json!({"data": {
                "id": 942432, "name": "BetterBags", "summary": "bags",
                "downloadCount": 1234.0, "authors": [{"name": "Cidan"}],
                "links": {"websiteUrl": "https://www.curseforge.com/wow/addons/better-bags"}
            }})))
            .mount(&server)
            .await;
        Mock::given(path("/v1/mods/942432/files"))
            .and(query_param("gameVersionTypeId", "99001"))
            .respond_with(ok(serde_json::json!({"data": [
                {"id": 2, "displayName": "v0.9.3-beta", "fileName": "b.zip", "releaseType": 2,
                 "fileDate": "2026-11-10T00:00:00Z", "fileLength": 10, "downloadUrl": "https://x/b.zip"},
                {"id": 1, "displayName": "v0.9.2", "fileName": "a.zip", "releaseType": 1,
                 "fileDate": "2026-11-07T00:00:00Z", "fileLength": 20, "downloadUrl": null}
            ]})))
            .mount(&server)
            .await;

        let http = Arc::new(Http::new(dir.path()).unwrap());
        let cf = CurseForge::new(http, server.uri(), Some("k".into()));
        let addon = AddonRef::new(SourceId::Curseforge, "942432");
        let rel = cf
            .resolve(&addon, &Flavor::Forever, Channel::Stable)
            .await
            .unwrap();
        assert_eq!(rel.version, "v0.9.2");
        assert_eq!(rel.download_url, None);
        assert_eq!(
            rel.page_url.as_deref(),
            Some("https://www.curseforge.com/wow/addons/better-bags")
        );
        let beta = cf
            .resolve(&addon, &Flavor::Forever, Channel::Beta)
            .await
            .unwrap();
        assert_eq!(beta.version, "v0.9.3-beta");
        assert_eq!(beta.size, Some(10));
    }
}
