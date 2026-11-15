//! WoWInterface through the public MMOUI v4 API (verified live Nov 2026).
//!
//! WoWInterface doesn't tag Forever as its own game version (no `1.60.x` in `gameVersions`),
//! so results are not filtered by flavor. Every file is a single stable release.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use kodo_core::addon::{AddonRef, Channel, Query, Release, RemoteAddon, SourceId};
use kodo_core::flavor::Flavor;
use kodo_core::source::{Progress, Source, SourceError};
use reqwest::header::HeaderMap;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::http::{Http, rfc3339_from_millis};

pub const API: &str = "https://api.mmoui.com/v4/game/WOW";
const LIST_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileEntry {
    id: u64,
    title: String,
    author: Option<String>,
    version: Option<String>,
    file_info_uri: Option<String>,
    #[serde(default)]
    downloads: u64,
    /// Unix milliseconds.
    last_update: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileDetails {
    id: u64,
    title: String,
    version: String,
    download_uri: String,
    change_log: Option<String>,
    last_update: Option<i64>,
}

pub struct Wowi {
    http: Arc<Http>,
    api: String,
    list_path: PathBuf,
    list: Mutex<Option<Arc<Vec<FileEntry>>>>,
}

impl Wowi {
    /// `cache_dir` is kodo's cache root; the daily file list lives under `catalogs/`.
    pub fn new(http: Arc<Http>, api: impl Into<String>, cache_dir: &Path) -> Self {
        Self {
            http,
            api: api.into().trim_end_matches('/').to_owned(),
            list_path: cache_dir.join("catalogs/wowi-filelist.json"),
            list: Mutex::new(None),
        }
    }

    /// The full file list, from memory, the on-disk copy if under a day old, or the API.
    async fn file_list(&self) -> Result<Arc<Vec<FileEntry>>, SourceError> {
        let mut guard = self.list.lock().await;
        if let Some(list) = guard.as_ref() {
            return Ok(list.clone());
        }
        let url = format!("{}/filelist.json", self.api);
        let fresh = tokio::fs::metadata(&self.list_path)
            .await
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < LIST_TTL);
        let cached = if fresh {
            tokio::fs::read(&self.list_path).await.ok()
        } else {
            None
        };
        let bytes = match cached {
            Some(b) => b,
            None => {
                let b = self
                    .http
                    .get_bytes(SourceId::Wowi, &url, HeaderMap::new(), false)
                    .await?;
                if let Some(dir) = self.list_path.parent() {
                    let _ = tokio::fs::create_dir_all(dir).await;
                }
                // Best effort; the in-memory copy covers this run either way.
                let _ = tokio::fs::write(&self.list_path, &b).await;
                b
            }
        };
        let list: Vec<FileEntry> =
            serde_json::from_slice(&bytes).map_err(|e| SourceError::Parse {
                url,
                message: e.to_string(),
            })?;
        let list = Arc::new(list);
        *guard = Some(list.clone());
        Ok(list)
    }
}

fn page_url(id: u64) -> String {
    format!("https://www.wowinterface.com/downloads/info{id}")
}

#[async_trait]
impl Source for Wowi {
    fn id(&self) -> SourceId {
        SourceId::Wowi
    }

    async fn search(&self, q: &Query, _flavor: &Flavor) -> Result<Vec<RemoteAddon>, SourceError> {
        // WoWI doesn't say which files run on which flavor, so browsing it would list
        // mostly retail addons; it only answers actual searches.
        if q.text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let list = self.file_list().await?;
        let mut hits: Vec<RemoteAddon> = list
            .iter()
            .filter(|f| q.matches(&[&f.title]))
            .map(|f| RemoteAddon {
                addon: AddonRef::new(SourceId::Wowi, f.id.to_string()),
                name: f.title.clone(),
                author: f.author.clone(),
                summary: None,
                downloads: Some(f.downloads),
                page_url: Some(f.file_info_uri.clone().unwrap_or_else(|| page_url(f.id))),
                version: f.version.clone(),
                updated: f
                    .last_update
                    .and_then(rfc3339_from_millis)
                    .map(|d| d.chars().take(10).collect()),
                tracks: Vec::new(),
            })
            .collect();
        q.rank(&mut hits);
        Ok(hits)
    }

    async fn resolve(
        &self,
        addon: &AddonRef,
        _flavor: &Flavor,
        _channel: Channel,
    ) -> Result<Release, SourceError> {
        if addon.source != SourceId::Wowi {
            return Err(SourceError::NotFound(addon.to_string()));
        }
        let url = format!("{}/filedetails/{}.json", self.api, addon.id);
        let details: Vec<FileDetails> = self
            .http
            .get_json(SourceId::Wowi, &url, HeaderMap::new(), true)
            .await
            .map_err(|e| match e {
                SourceError::Http { status: 404, .. } => SourceError::NotFound(addon.to_string()),
                e => e,
            })?;
        let d = details
            .into_iter()
            .next()
            .ok_or_else(|| SourceError::NotFound(addon.to_string()))?;
        Ok(Release {
            addon: addon.clone(),
            name: d.title,
            version: d.version,
            channel: Channel::Stable,
            download_url: Some(d.download_uri),
            page_url: Some(page_url(d.id)),
            size: None,
            changelog: d.change_log.filter(|c| !c.trim().is_empty()),
            published_at: d.last_update.and_then(rfc3339_from_millis),
        })
    }

    async fn download(
        &self,
        url: &str,
        dest: &Path,
        progress: Option<Progress>,
    ) -> Result<u64, SourceError> {
        self.http
            .download(SourceId::Wowi, url, dest, progress)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kodo_core::addon::{AddonRef, Channel, Query, RemoteAddon, Sort, SourceId};
    use kodo_core::flavor::Flavor;
    use kodo_core::source::{Source, SourceError};
    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::Http;
    use crate::wowi::Wowi;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/sources")
                .join(name),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn searches_cached_list_and_resolves() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        Mock::given(path("/filelist.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture("wowi-filelist.json")))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/filedetails/3896.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(fixture("wowi-filedetails-3896.json")),
            )
            .mount(&server)
            .await;
        Mock::given(path("/filedetails/1.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let http = Arc::new(Http::new(dir.path()).unwrap());
        let wowi = Wowi::new(http.clone(), server.uri(), dir.path());
        let q = Query {
            text: "ATLAS".into(),
            limit: 10,
            ..Query::default()
        };
        let names = |hits: Vec<RemoteAddon>| hits.into_iter().map(|h| h.name).collect::<Vec<_>>();
        let hits = wowi.search(&q, &Flavor::Forever).await.unwrap();
        assert_eq!(
            names(hits),
            ["Atlas", "AtlasLoot Enhanced"],
            "exact name first"
        );
        let by_downloads = Query {
            sort: Sort::Downloads,
            ..q.clone()
        };
        let hits = wowi.search(&by_downloads, &Flavor::Forever).await.unwrap();
        assert_eq!(names(hits), ["AtlasLoot Enhanced", "Atlas"]);
        assert!(
            wowi.search(&Query::default(), &Flavor::Forever)
                .await
                .unwrap()
                .is_empty(),
            "WoWI doesn't browse"
        );

        // A second instance reads the day-old-or-newer copy from disk, not the API.
        let again = Wowi::new(http, server.uri(), dir.path());
        assert_eq!(again.search(&q, &Flavor::Forever).await.unwrap().len(), 2);

        let rel = wowi
            .resolve(
                &AddonRef::new(SourceId::Wowi, "3896"),
                &Flavor::Forever,
                Channel::Stable,
            )
            .await
            .unwrap();
        assert_eq!(rel.version, "v1.54.07");
        assert_eq!(rel.channel, Channel::Stable);
        assert!(rel.download_url.unwrap().contains("getfile.php?id=3896"));
        assert_eq!(rel.published_at.as_deref(), Some("2026-11-11T17:06:24Z"));

        let err = wowi
            .resolve(
                &AddonRef::new(SourceId::Wowi, "1"),
                &Flavor::Forever,
                Channel::Stable,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SourceError::NotFound(_)));
    }
}
