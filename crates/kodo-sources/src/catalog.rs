//! The strongbox GitHub catalogue (ogri-la/strongbox-catalogue, public domain, rebuilt daily):
//! every GitHub-hosted addon it knows, with the game tracks each release supports.
//!
//! Fetched at runtime from a list of URLs (kodo's mirror first, upstream as fallback), kept on
//! disk for a day, and revalidated with ETags. A stale disk copy beats no catalogue at all.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use kodo_core::addon::SourceId;
use kodo_core::flavor::Flavor;
use kodo_core::source::SourceError;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::http::Http;

/// kodo's mirror of the upstream file, refreshed by a cron job.
pub const MIRROR_URL: &str = "https://static.g7c.us/kodo/github-catalogue.json";
pub const UPSTREAM_URL: &str =
    "https://raw.githubusercontent.com/ogri-la/strongbox-catalogue/master/github-catalogue.json";
const TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    /// `owner/repo`.
    #[serde(rename = "source-id")]
    pub repo: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "download-count")]
    pub downloads: u64,
    #[serde(default, rename = "game-track-list")]
    pub tracks: Vec<String>,
    pub url: String,
    /// RFC 3339 timestamp of the latest release.
    #[serde(default, rename = "updated-date")]
    pub updated: String,
}

impl Entry {
    /// Whether the catalogue lists a build for the flavor. Flavors it has no track for match.
    pub fn supports(&self, flavor: &Flavor) -> bool {
        let tracks = flavor.catalog_tracks();
        tracks.is_empty() || self.tracks.iter().any(|t| tracks.contains(&t.as_str()))
    }
}

#[derive(Deserialize)]
struct File {
    #[serde(rename = "addon-summary-list")]
    addons: Vec<Entry>,
}

pub struct Catalog {
    http: Arc<Http>,
    urls: Vec<String>,
    path: PathBuf,
    loaded: Mutex<Option<(Instant, Arc<Vec<Entry>>)>>,
}

impl Catalog {
    /// `urls` are tried in order; `cache_dir` is kodo's cache root.
    pub fn new(http: Arc<Http>, urls: Vec<String>, cache_dir: &std::path::Path) -> Self {
        Self {
            http,
            urls,
            path: cache_dir.join("catalogs/github-catalogue.json"),
            loaded: Mutex::new(None),
        }
    }

    /// The catalogue, reloading when the in-memory or on-disk copy is over a day old.
    pub async fn entries(&self) -> Result<Arc<Vec<Entry>>, SourceError> {
        let mut guard = self.loaded.lock().await;
        if let Some((at, list)) = guard.as_ref()
            && at.elapsed() < TTL
        {
            return Ok(list.clone());
        }
        let disk_age = tokio::fs::metadata(&self.path)
            .await
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok());
        let fresh_on_disk = match disk_age {
            Some(age) if age < TTL => tokio::fs::read(&self.path).await.ok(),
            _ => None,
        };
        let bytes = match fresh_on_disk {
            Some(b) => b,
            None => match self.fetch().await {
                Ok(b) => {
                    if let Some(dir) = self.path.parent() {
                        let _ = tokio::fs::create_dir_all(dir).await;
                    }
                    // Best effort; the in-memory copy covers this run either way.
                    let _ = tokio::fs::write(&self.path, &b).await;
                    b
                }
                Err(e) => tokio::fs::read(&self.path).await.map_err(|_| e)?,
            },
        };
        let file: File = serde_json::from_slice(&bytes).map_err(|e| SourceError::Parse {
            url: self.urls.first().cloned().unwrap_or_default(),
            message: e.to_string(),
        })?;
        let list = Arc::new(file.addons);
        *guard = Some((Instant::now(), list.clone()));
        Ok(list)
    }

    /// First URL that answers wins; the last error is reported if none do.
    async fn fetch(&self) -> Result<Vec<u8>, SourceError> {
        let mut last = SourceError::NotFound("github catalogue (no URLs configured)".into());
        for url in &self.urls {
            match self
                .http
                .get_bytes(SourceId::Github, url, HeaderMap::new(), true)
                .await
            {
                Ok(b) => return Ok(b),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

/// Lowercase alphanumerics only, so `Better Bags`, `BetterBags` and `better-bags` compare equal.
pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kodo_core::flavor::Flavor;
    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::catalog::Catalog;
    use crate::http::Http;

    fn fixture() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/sources/strongbox-github-catalogue.json"),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn falls_back_to_upstream_then_disk() {
        let server = MockServer::start().await;
        Mock::given(path("/mirror.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(path("/upstream.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture()))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let http = Arc::new(Http::new(dir.path()).unwrap());
        let urls = vec![
            format!("{}/mirror.json", server.uri()),
            format!("{}/upstream.json", server.uri()),
        ];

        let catalog = Catalog::new(http.clone(), urls.clone(), dir.path());
        let entries = catalog.entries().await.unwrap();
        let bb = entries
            .iter()
            .find(|e| e.repo == "Cidan/BetterBags")
            .unwrap();
        assert!(bb.supports(&Flavor::Forever));
        assert!(
            !entries
                .iter()
                .any(|e| e.repo == "0kamiyasha/TOSUI" && e.supports(&Flavor::Forever))
        );

        // A second instance reads the fresh disk copy without touching the network.
        let again = Catalog::new(
            http,
            vec!["http://127.0.0.1:9/unreachable".into()],
            dir.path(),
        );
        assert_eq!(again.entries().await.unwrap().len(), entries.len());
    }
}
