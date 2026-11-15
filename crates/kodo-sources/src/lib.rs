//! Addon source adapters behind `kodo_core::source::Source`.
//!
//! Frontends call [`build`] once with their settings and hand the result to the core engine;
//! nothing else in this crate needs to be touched directly.

pub mod catalog;
pub mod curseforge;
pub mod github;
pub mod http;
pub mod wowi;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kodo_core::addon::SourceId;
use kodo_core::config::{Config, Paths, Secret};
use kodo_core::source::{Source, SourceError};

use crate::catalog::Catalog;
use crate::curseforge::CurseForge;
use crate::github::{GitHub, KodoRelease};
use crate::http::Http;
use crate::wowi::Wowi;

#[derive(Debug, Clone, Default)]
pub struct SourcesOptions {
    /// kodo's cache root; sources keep `http/` (ETag cache) and `catalogs/` under it.
    pub cache_dir: PathBuf,
    pub github: bool,
    pub wowinterface: bool,
    pub curseforge: bool,
    pub github_token: Option<String>,
    pub curseforge_key: Option<String>,
    /// Where to fetch the GitHub catalogue first; defaults to kodo's mirror. Upstream is always
    /// tried next.
    pub catalog_url: Option<String>,
}

impl SourcesOptions {
    /// The options a frontend uses: config toggles, plus secrets from env or the OS keychain.
    /// Blocking (keychain access).
    pub fn from_config(paths: &Paths, config: &Config) -> Self {
        Self {
            cache_dir: paths.source_cache(),
            github: config.sources.github,
            wowinterface: config.sources.wowinterface,
            curseforge: config.sources.curseforge,
            github_token: Secret::GithubToken.get(),
            curseforge_key: Secret::CurseforgeKey.get(),
            catalog_url: config.sources.catalog_url.clone(),
        }
    }
}

/// Build every enabled source, sharing one HTTP client and cache.
///
/// CurseForge is included whenever it is enabled, even without a key, so its calls report
/// `SourceError::MissingKey` instead of the source silently vanishing.
pub fn build(opts: &SourcesOptions) -> Result<Vec<Arc<dyn Source>>, SourceError> {
    let http = Arc::new(Http::new(&opts.cache_dir)?);
    let mut sources: Vec<Arc<dyn Source>> = Vec::new();
    if opts.github {
        let urls = vec![
            opts.catalog_url
                .clone()
                .unwrap_or_else(|| catalog::MIRROR_URL.to_owned()),
            catalog::UPSTREAM_URL.to_owned(),
        ];
        let catalog = Arc::new(Catalog::new(http.clone(), urls, &opts.cache_dir));
        sources.push(Arc::new(GitHub::new(
            http.clone(),
            github::API,
            opts.github_token.clone(),
            catalog,
        )));
    }
    if opts.wowinterface {
        sources.push(Arc::new(Wowi::new(
            http.clone(),
            wowi::API,
            &opts.cache_dir,
        )));
    }
    if opts.curseforge {
        sources.push(Arc::new(CurseForge::new(
            http,
            curseforge::API,
            opts.curseforge_key.clone(),
        )));
    }
    Ok(sources)
}

/// Whether a kodo release newer than `current` is out. `cache_dir` is kodo's cache root.
pub async fn check_kodo(
    cache_dir: &Path,
    current: &str,
) -> Result<Option<KodoRelease>, SourceError> {
    let http = Http::new(cache_dir)?;
    github::newer_kodo(&http, github::KODO_LATEST, current).await
}

/// Biggest README image fetched.
const IMAGE_MAX: usize = 4 * 1024 * 1024;

/// README images, cached on disk like API responses. Best effort: anything that fails, is too
/// big, or isn't PNG, JPEG, GIF or WebP (shields.io SVG badges, say) is left out.
pub async fn fetch_images(cache_dir: &Path, urls: Vec<String>) -> Vec<(String, Vec<u8>)> {
    let Ok(http) = Http::new(cache_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for url in urls {
        let got = http
            .get_bytes(
                SourceId::Github,
                &url,
                reqwest::header::HeaderMap::new(),
                true,
            )
            .await;
        if let Ok(bytes) = got
            && bytes.len() <= IMAGE_MAX
            && is_raster(&bytes)
        {
            out.push((url, bytes));
        }
    }
    out
}

fn is_raster(b: &[u8]) -> bool {
    b.starts_with(b"\x89PNG")
        || b.starts_with(&[0xFF, 0xD8, 0xFF])
        || b.starts_with(b"GIF8")
        || (b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP"))
}

#[cfg(test)]
mod tests {
    use kodo_core::addon::SourceId;

    use crate::{SourcesOptions, build};

    #[test]
    fn builds_enabled_sources() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SourcesOptions {
            cache_dir: dir.path().into(),
            github: true,
            wowinterface: false,
            curseforge: true,
            ..SourcesOptions::default()
        };
        let ids: Vec<_> = build(&opts).unwrap().iter().map(|s| s.id()).collect();
        assert_eq!(ids, [SourceId::Github, SourceId::Curseforge]);
    }
}
