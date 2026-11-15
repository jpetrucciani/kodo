//! Shared HTTP plumbing: clients with timeouts, an on-disk ETag cache, streaming downloads.

use std::path::{Path, PathBuf};
use std::time::Duration;

use kodo_core::addon::SourceId;
use kodo_core::source::{Progress, SourceError};
use reqwest::header::{ETAG, HeaderMap, HeaderValue, IF_NONE_MATCH};
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use tokio::io::AsyncWriteExt;

const USER_AGENT: &str = concat!("kodo/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole-request limit for API calls.
const API_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-read limit for downloads, so large files on slow links still finish.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Http {
    api: reqwest::Client,
    downloads: reqwest::Client,
    /// Holds `<fnv>.etag` / `<fnv>.body` pairs.
    etag_dir: PathBuf,
}

impl Http {
    /// `cache_dir` is kodo's cache root; responses are cached under `cache_dir/http/`.
    pub fn new(cache_dir: &Path) -> Result<Self, SourceError> {
        let build = |b: reqwest::ClientBuilder| {
            b.user_agent(USER_AGENT)
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .map_err(|e| SourceError::Network {
                    url: String::new(),
                    message: format!("building HTTP client: {e}"),
                })
        };
        Ok(Self {
            api: build(reqwest::Client::builder().timeout(API_TIMEOUT))?,
            downloads: build(reqwest::Client::builder().read_timeout(READ_TIMEOUT))?,
            etag_dir: cache_dir.join("http"),
        })
    }

    pub async fn get_json<T: DeserializeOwned>(
        &self,
        source_id: SourceId,
        url: &str,
        headers: HeaderMap,
        use_etag: bool,
    ) -> Result<T, SourceError> {
        let bytes = self.get_bytes(source_id, url, headers, use_etag).await?;
        serde_json::from_slice(&bytes).map_err(|e| SourceError::Parse {
            url: url.to_owned(),
            message: e.to_string(),
        })
    }

    /// GET a body. With `use_etag`, a cached ETag is sent as `If-None-Match` and the cached
    /// body is returned on `304`, which GitHub does not count against the rate limit.
    pub async fn get_bytes(
        &self,
        source_id: SourceId,
        url: &str,
        mut headers: HeaderMap,
        use_etag: bool,
    ) -> Result<Vec<u8>, SourceError> {
        let cache = use_etag.then(|| self.cache_paths(url));
        if let Some((etag_path, body_path)) = &cache
            && body_path.is_file()
            && let Ok(tag) = tokio::fs::read_to_string(etag_path).await
            && let Ok(value) = HeaderValue::from_str(tag.trim())
        {
            headers.insert(IF_NONE_MATCH, value);
        }
        let resp = self
            .api
            .get(url)
            .headers(headers)
            .send()
            .await
            .map_err(network(url))?;
        if resp.status() == StatusCode::NOT_MODIFIED
            && let Some((_, body_path)) = &cache
        {
            return tokio::fs::read(body_path).await.map_err(io(body_path));
        }
        let resp = check(source_id, url, resp)?;
        let etag = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = resp.bytes().await.map_err(network(url))?.to_vec();
        if let (Some((etag_path, body_path)), Some(etag)) = (&cache, etag) {
            // Best effort: a cache that can't be written only costs a full fetch next time.
            if write_atomic(body_path, &bytes).await.is_ok() {
                let _ = write_atomic(etag_path, etag.as_bytes()).await;
            }
        }
        Ok(bytes)
    }

    /// Stream `url` into `dest` through a temp file in the same directory, then rename.
    pub async fn download(
        &self,
        source_id: SourceId,
        url: &str,
        dest: &Path,
        progress: Option<Progress>,
    ) -> Result<u64, SourceError> {
        let resp = self.downloads.get(url).send().await.map_err(network(url))?;
        let mut resp = check(source_id, url, resp)?;
        let total = resp.content_length();
        let part = part_path(dest);
        if let Some(dir) = dest.parent() {
            tokio::fs::create_dir_all(dir).await.map_err(io(dir))?;
        }
        let result = async {
            let mut file = tokio::fs::File::create(&part).await.map_err(io(&part))?;
            let mut written = 0u64;
            while let Some(chunk) = resp.chunk().await.map_err(network(url))? {
                file.write_all(&chunk).await.map_err(io(&part))?;
                written += chunk.len() as u64;
                if let Some(p) = &progress {
                    p(written, total);
                }
            }
            file.flush().await.map_err(io(&part))?;
            drop(file);
            if let Some(total) = total
                && total != written
            {
                return Err(SourceError::Network {
                    url: url.to_owned(),
                    message: format!("download truncated at {written} of {total} bytes"),
                });
            }
            tokio::fs::rename(&part, dest).await.map_err(io(dest))?;
            Ok(written)
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&part).await;
        }
        result
    }

    fn cache_paths(&self, url: &str) -> (PathBuf, PathBuf) {
        let key = format!("{:016x}", kodo_core::fnv1a(url.as_bytes()));
        (
            self.etag_dir.join(format!("{key}.etag")),
            self.etag_dir.join(format!("{key}.body")),
        )
    }
}

/// Turn a non-success response into the matching `SourceError`.
fn check(source_id: SourceId, url: &str, resp: Response) -> Result<Response, SourceError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let exhausted = resp
        .headers()
        .get("x-ratelimit-remaining")
        .is_some_and(|v| v.as_bytes() == b"0");
    if status == StatusCode::TOO_MANY_REQUESTS || (status == StatusCode::FORBIDDEN && exhausted) {
        let hint = match source_id {
            SourceId::Github => "set GITHUB_TOKEN to raise the limit, or try again later",
            _ => "try again later",
        };
        return Err(SourceError::RateLimited {
            source_id,
            hint: hint.to_owned(),
        });
    }
    Err(SourceError::Http {
        url: url.to_owned(),
        status: status.as_u16(),
    })
}

fn part_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dest.with_file_name(format!(".{name}.{}.part", std::process::id()))
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), SourceError> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(io(dir))?;
    }
    let part = part_path(path);
    tokio::fs::write(&part, bytes).await.map_err(io(&part))?;
    tokio::fs::rename(&part, path).await.map_err(io(path))
}

pub(crate) fn network(url: &str) -> impl FnOnce(reqwest::Error) -> SourceError {
    let url = url.to_owned();
    move |e| SourceError::Network {
        url,
        message: e.to_string(),
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> SourceError {
    let path = path.to_path_buf();
    move |err| SourceError::Io { path, err }
}

/// Epoch milliseconds as an RFC 3339 UTC timestamp; `None` before 1970.
pub(crate) fn rfc3339_from_millis(ms: i64) -> Option<String> {
    u64::try_from(ms.div_euclid(1000))
        .ok()
        .map(kodo_core::lockfile::rfc3339)
}

#[cfg(test)]
mod tests {
    use kodo_core::addon::SourceId;
    use kodo_core::source::SourceError;
    use reqwest::header::HeaderMap;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::Http;

    #[tokio::test]
    async fn etag_304_reuses_cached_body() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let http = Http::new(dir.path()).unwrap();
        let url = format!("{}/thing", server.uri());

        Mock::given(method("GET"))
            .and(path("/thing"))
            .and(header("if-none-match", "\"v1\""))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/thing"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("etag", "\"v1\"")
                    .set_body_string("hello"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let first = http
            .get_bytes(SourceId::Github, &url, HeaderMap::new(), true)
            .await
            .unwrap();
        let second = http
            .get_bytes(SourceId::Github, &url, HeaderMap::new(), true)
            .await
            .unwrap();
        assert_eq!(first, b"hello");
        assert_eq!(second, b"hello");
    }

    #[tokio::test]
    async fn maps_rate_limit_and_downloads_atomically() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let http = Http::new(dir.path()).unwrap();
        Mock::given(path("/limited"))
            .respond_with(ResponseTemplate::new(403).insert_header("x-ratelimit-remaining", "0"))
            .mount(&server)
            .await;
        Mock::given(path("/file.zip"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![7u8; 4096]))
            .mount(&server)
            .await;

        let err = http
            .get_bytes(
                SourceId::Github,
                &format!("{}/limited", server.uri()),
                HeaderMap::new(),
                false,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            SourceError::RateLimited {
                source_id: SourceId::Github,
                ..
            }
        ));

        let dest = dir.path().join("dl/file.zip");
        let n = http
            .download(
                SourceId::Github,
                &format!("{}/file.zip", server.uri()),
                &dest,
                None,
            )
            .await
            .unwrap();
        assert_eq!(n, 4096);
        assert_eq!(std::fs::read(&dest).unwrap().len(), 4096);
        assert_eq!(
            std::fs::read_dir(dest.parent().unwrap()).unwrap().count(),
            1,
            "no .part left"
        );
    }
}
