//! GitHub releases: search and suggestions from the strongbox catalogue, BigWigs-packager
//! asset picking for installs.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use kodo_core::addon::{AddonRef, Change, Channel, Match, Query, Release, RemoteAddon, SourceId};
use kodo_core::flavor::Flavor;
use kodo_core::local::LocalAddon;
use kodo_core::source::{Progress, Source, SourceError};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use serde::Deserialize;

use crate::catalog::{Catalog, Entry as CatalogEntry, normalize};
use crate::http::Http;

pub const API: &str = "https://api.github.com";
const CHANGELOG_MAX: usize = 4096;

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    name: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    html_url: String,
    published_at: Option<String>,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

/// The packager's `release.json` (BigWigs packager / ogri-la spec).
#[derive(Debug, Deserialize)]
struct ReleaseJson {
    releases: Vec<ReleaseJsonEntry>,
}

#[derive(Debug, Deserialize)]
struct ReleaseJsonEntry {
    filename: String,
    #[serde(default)]
    nolib: bool,
    #[serde(default)]
    metadata: Vec<ReleaseJsonMeta>,
}

#[derive(Debug, Deserialize)]
struct ReleaseJsonMeta {
    flavor: String,
    interface: u32,
}

pub struct GitHub {
    http: Arc<Http>,
    api: String,
    token: Option<String>,
    catalog: Arc<Catalog>,
}

impl GitHub {
    pub fn new(
        http: Arc<Http>,
        api: impl Into<String>,
        token: Option<String>,
        catalog: Arc<Catalog>,
    ) -> Self {
        Self {
            http,
            api: api.into().trim_end_matches('/').to_owned(),
            token,
            catalog,
        }
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        h.insert(
            "x-github-api-version",
            HeaderValue::from_static("2022-11-28"),
        );
        if let Some(value) = self
            .token
            .as_ref()
            .and_then(|t| HeaderValue::from_str(&format!("Bearer {t}")).ok())
        {
            h.insert(AUTHORIZATION, value);
        }
        h
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        addon: &AddonRef,
    ) -> Result<T, SourceError> {
        self.http
            .get_json(SourceId::Github, url, self.headers(), true)
            .await
            .map_err(|e| match e {
                SourceError::Http { status: 404, .. } => SourceError::NotFound(addon.to_string()),
                e => e,
            })
    }

    /// Recent releases, newest first (one request, shared with update checks via the ETag cache).
    async fn releases(&self, addon: &AddonRef) -> Result<Vec<GhRelease>, SourceError> {
        let url = format!(
            "{}/repos/{}/releases?per_page=20",
            self.api,
            repo_ref(addon)?
        );
        let all: Vec<GhRelease> = self.get(&url, addon).await?;
        Ok(all.into_iter().filter(|r| !r.draft).collect())
    }

    async fn build_release(
        &self,
        addon: &AddonRef,
        flavor: &Flavor,
        rel: GhRelease,
    ) -> Result<Release, SourceError> {
        let asset = self.pick_asset(addon, flavor, &rel).await?;
        let channel = channel_of(&rel);
        let repo_name = addon.id.rsplit('/').next().unwrap_or(&addon.id).to_owned();
        Ok(Release {
            addon: addon.clone(),
            name: repo_name,
            version: rel.tag_name,
            channel,
            download_url: Some(asset.browser_download_url),
            page_url: Some(rel.html_url),
            size: Some(asset.size),
            changelog: rel
                .body
                .map(|b| truncate(b.trim(), CHANGELOG_MAX))
                .filter(|b| !b.is_empty()),
            published_at: rel.published_at,
        })
    }

    /// Prefer the packager's `release.json`; otherwise guess from zip file names.
    async fn pick_asset(
        &self,
        addon: &AddonRef,
        flavor: &Flavor,
        rel: &GhRelease,
    ) -> Result<GhAsset, SourceError> {
        let no_asset = || SourceError::NoAsset {
            addon: addon.clone(),
            version: rel.tag_name.clone(),
            flavor: flavor.label().to_owned(),
        };
        if let Some(manifest) = rel.assets.iter().find(|a| a.name == "release.json") {
            let parsed: Result<ReleaseJson, _> = self
                .http
                .get_json(
                    SourceId::Github,
                    &manifest.browser_download_url,
                    HeaderMap::new(),
                    true,
                )
                .await;
            match parsed {
                Ok(manifest) => {
                    return pick_from_manifest(&manifest, flavor)
                        .and_then(|file| rel.assets.iter().find(|a| a.name == file))
                        .cloned()
                        .ok_or_else(no_asset);
                }
                // A malformed manifest falls back to file names; network failures don't.
                Err(SourceError::Parse { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        pick_by_name(&rel.assets, flavor)
            .cloned()
            .ok_or_else(no_asset)
    }
}

/// kodo's own latest release.
pub const KODO_LATEST: &str = "https://api.github.com/repos/jpetrucciani/kodo/releases/latest";

/// A kodo build newer than the running one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KodoRelease {
    pub version: String,
    /// Release page with the downloads.
    pub url: String,
}

/// The latest kodo release at `url` if it's newer than `current`; `None` when up to date or
/// there are no releases yet.
pub async fn newer_kodo(
    http: &Http,
    url: &str,
    current: &str,
) -> Result<Option<KodoRelease>, SourceError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    let rel: GhRelease = match http.get_json(SourceId::Github, url, headers, true).await {
        Ok(r) => r,
        Err(SourceError::Http { status: 404, .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(is_newer(&rel.tag_name, current).then(|| KodoRelease {
        version: rel.tag_name.trim_start_matches('v').to_owned(),
        url: rel.html_url,
    }))
}

/// Compare dotted release numbers (`v0.3.1` vs `0.2.9`). With equal numbers a final release
/// beats a pre-release (`0.1.0` > `0.1.0-rc.1`); pre-releases aren't ordered among themselves.
fn is_newer(tag: &str, current: &str) -> bool {
    let key = |v: &str| -> (Vec<u64>, bool) {
        let v = v.trim_start_matches('v');
        let v = v.split('+').next().unwrap_or(v);
        let (nums, pre) = v.split_once('-').map_or((v, false), |(n, _)| (n, true));
        let nums = nums.split('.').map_while(|p| p.parse().ok()).collect();
        (nums, !pre)
    };
    key(tag) > key(current)
}

fn remote(e: &CatalogEntry) -> RemoteAddon {
    RemoteAddon {
        addon: AddonRef::new(SourceId::Github, e.repo.clone()),
        name: e.label.clone(),
        author: e.repo.split('/').next().map(str::to_owned),
        summary: e.description.clone(),
        downloads: Some(e.downloads),
        page_url: Some(e.url.clone()),
        version: None,
        updated: e.updated.get(..10).map(str::to_owned),
        tracks: e.tracks.clone(),
    }
}

fn pick_from_manifest<'a>(manifest: &'a ReleaseJson, flavor: &Flavor) -> Option<&'a str> {
    let range = flavor.interfaces();
    let flavors = flavor.release_flavors();
    manifest
        .releases
        .iter()
        .filter(|r| {
            r.metadata.iter().any(|m| {
                flavors.iter().any(|f| m.flavor.eq_ignore_ascii_case(f))
                    || range.as_ref().is_some_and(|r| r.contains(&m.interface))
            })
        })
        .min_by_key(|r| r.nolib)
        .map(|r| r.filename.as_str())
}

/// Zip tagged for the flavor, else an untagged zip, else the only zip. `-nolib` builds lose ties.
fn pick_by_name<'a>(assets: &'a [GhAsset], flavor: &Flavor) -> Option<&'a GhAsset> {
    let mut zips: Vec<(&GhAsset, String)> = assets
        .iter()
        .map(|a| (a, a.name.to_ascii_lowercase()))
        .filter(|(_, n)| n.ends_with(".zip"))
        .collect();
    zips.sort_by_key(|(_, n)| n.contains("nolib"));
    let tagged = zips
        .iter()
        .find(|(_, n)| flavor.zip_markers().iter().any(|m| n.contains(m)));
    let untagged = || {
        zips.iter()
            .find(|(_, n)| !Flavor::all_zip_markers().any(|m| n.contains(m)))
    };
    let only = || (zips.len() == 1).then(|| &zips[0]);
    tagged.or_else(untagged).or_else(only).map(|(a, _)| *a)
}

fn channel_of(rel: &GhRelease) -> Channel {
    let label = format!(
        "{} {}",
        rel.tag_name,
        rel.name.as_deref().unwrap_or_default()
    )
    .to_ascii_lowercase();
    if label.contains("alpha") {
        Channel::Alpha
    } else if rel.prerelease || label.contains("beta") {
        Channel::Beta
    } else {
        Channel::Stable
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn repo_ref(addon: &AddonRef) -> Result<&str, SourceError> {
    if addon.source == SourceId::Github {
        Ok(&addon.id)
    } else {
        Err(SourceError::NotFound(addon.to_string()))
    }
}

#[async_trait]
impl Source for GitHub {
    fn id(&self) -> SourceId {
        SourceId::Github
    }

    async fn search(&self, q: &Query, flavor: &Flavor) -> Result<Vec<RemoteAddon>, SourceError> {
        let entries = self.catalog.entries().await?;
        let mut hits: Vec<RemoteAddon> = entries
            .iter()
            .filter(|e| e.supports(flavor))
            .filter(|e| {
                q.matches(&[
                    &e.label,
                    &e.repo,
                    e.description.as_deref().unwrap_or_default(),
                ])
            })
            .map(remote)
            .collect();
        q.rank(&mut hits);
        Ok(hits)
    }

    /// Match unclaimed folders by folder name or TOC title against catalogue labels and repo
    /// names, preferring the most downloaded entry when forks share a name.
    async fn suggest(
        &self,
        local: &[LocalAddon],
        flavor: &Flavor,
    ) -> Result<Vec<Match>, SourceError> {
        let entries = self.catalog.entries().await?;
        let mut best: std::collections::HashMap<String, &CatalogEntry> =
            std::collections::HashMap::new();
        for e in entries.iter().filter(|e| e.supports(flavor)) {
            let repo_name = e.repo.rsplit('/').next().unwrap_or(&e.repo);
            for key in [normalize(&e.label), normalize(repo_name)] {
                let slot = best.entry(key).or_insert(e);
                if e.downloads > slot.downloads {
                    *slot = e;
                }
            }
        }
        Ok(local
            .iter()
            .filter_map(|l| {
                let title = l.toc.as_ref().and_then(|t| t.title.as_deref());
                let hit = [Some(l.folder.as_str()), title]
                    .into_iter()
                    .flatten()
                    .find_map(|k| best.get(&normalize(k)))?;
                Some(Match {
                    folders: vec![l.folder.clone()],
                    addon: AddonRef::new(SourceId::Github, hit.repo.clone()),
                    version: None,
                })
            })
            .collect())
    }

    async fn resolve(
        &self,
        addon: &AddonRef,
        flavor: &Flavor,
        channel: Channel,
    ) -> Result<Release, SourceError> {
        let rel = self
            .releases(addon)
            .await?
            .into_iter()
            .find(|r| channel.accepts(channel_of(r)))
            .ok_or_else(|| SourceError::NoRelease(addon.clone()))?;
        self.build_release(addon, flavor, rel).await
    }

    async fn changes(
        &self,
        addon: &AddonRef,
        channel: Channel,
        since: &str,
    ) -> Result<Option<Vec<Change>>, SourceError> {
        let newer = self
            .releases(addon)
            .await?
            .into_iter()
            .filter(|r| channel.accepts(channel_of(r)))
            .take_while(|r| r.tag_name != since)
            .map(|r| Change {
                version: r.tag_name,
                published_at: r.published_at,
                notes: r
                    .body
                    .map(|b| truncate(b.trim(), CHANGELOG_MAX))
                    .filter(|b| !b.is_empty()),
            })
            .collect();
        Ok(Some(newer))
    }

    async fn readme(&self, addon: &AddonRef) -> Result<Option<String>, SourceError> {
        let url = format!("{}/repos/{}/readme", self.api, repo_ref(addon)?);
        let mut headers = self.headers();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github.raw"),
        );
        match self
            .http
            .get_bytes(SourceId::Github, &url, headers, true)
            .await
        {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(SourceError::Http { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn resolve_version(
        &self,
        addon: &AddonRef,
        flavor: &Flavor,
        version: &str,
    ) -> Result<Release, SourceError> {
        let repo = repo_ref(addon)?;
        let url = format!("{}/repos/{repo}/releases/tags/{version}", self.api);
        let rel: GhRelease = self.get(&url, addon).await.map_err(|e| match e {
            SourceError::NotFound(a) => SourceError::NotFound(format!("{a} {version}")),
            e => e,
        })?;
        self.build_release(addon, flavor, rel).await
    }

    async fn download(
        &self,
        url: &str,
        dest: &Path,
        progress: Option<Progress>,
    ) -> Result<u64, SourceError> {
        self.http
            .download(SourceId::Github, url, dest, progress)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kodo_core::addon::{AddonRef, Channel, Query, Sort, SourceId};
    use kodo_core::flavor::Flavor;
    use kodo_core::local::LocalAddon;
    use kodo_core::source::{Source, SourceError};
    use kodo_core::toc::Toc;
    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::catalog::Catalog;
    use crate::github::{GhAsset, GitHub, is_newer, pick_by_name};
    use crate::http::Http;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/sources")
                .join(name),
        )
        .unwrap()
    }

    async fn setup() -> (MockServer, GitHub, tempfile::TempDir) {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let http = Arc::new(Http::new(dir.path()).unwrap());
        Mock::given(path("/catalogue.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture("strongbox-github-catalogue.json")),
            )
            .mount(&server)
            .await;
        let catalog = Catalog::new(
            http.clone(),
            vec![format!("{}/catalogue.json", server.uri())],
            dir.path(),
        );
        let gh = GitHub::new(http, server.uri(), None, Arc::new(catalog));
        (server, gh, dir)
    }

    fn bb() -> AddonRef {
        AddonRef::new(SourceId::Github, "Cidan/BetterBags")
    }

    #[tokio::test]
    async fn resolves_forever_asset_from_release_json() {
        let (server, gh, _dir) = setup().await;
        let releases = fixture("betterbags-releases.json").replace("{{base}}", &server.uri());
        Mock::given(path("/repos/Cidan/BetterBags/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_string(releases))
            .mount(&server)
            .await;
        Mock::given(path(
            "/Cidan/BetterBags/releases/download/v0.9.2/release.json",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(fixture("betterbags-release.json")),
        )
        .expect(2)
        .mount(&server)
        .await;

        let rel = gh
            .resolve(&bb(), &Flavor::Forever, Channel::Stable)
            .await
            .unwrap();
        assert_eq!(rel.version, "v0.9.2");
        assert_eq!(rel.channel, Channel::Stable);
        assert_eq!(rel.size, Some(3_354_981));
        assert!(
            rel.download_url
                .unwrap()
                .ends_with("/v0.9.2/BetterBags-v0.9.2.zip")
        );

        // The manifest is authoritative: a flavor it doesn't list gets no asset.
        let err = gh
            .resolve(&bb(), &Flavor::Other("ptr".into()), Channel::Stable)
            .await
            .unwrap_err();
        assert!(matches!(err, SourceError::NoAsset { .. }), "{err}");
    }

    #[tokio::test]
    async fn channel_picks_newest_accepted_release() {
        let (server, gh, _dir) = setup().await;
        let base = server.uri();
        let body = serde_json::json!([
            {"tag_name": "v2.0.0-alpha1", "name": null, "draft": false, "prerelease": true,
             "html_url": "h", "published_at": null, "body": null,
             "assets": [{"name": "A-v2a.zip", "size": 1, "browser_download_url": format!("{base}/a")}]},
            {"tag_name": "v1.1.0-beta2", "name": null, "draft": false, "prerelease": true,
             "html_url": "h", "published_at": null, "body": null,
             "assets": [{"name": "A-v1.1b.zip", "size": 1, "browser_download_url": format!("{base}/b")}]},
            {"tag_name": "v1.2.0", "name": null, "draft": true, "prerelease": false,
             "html_url": "h", "published_at": null, "body": null, "assets": []},
            {"tag_name": "v1.0.0", "name": null, "draft": false, "prerelease": false,
             "html_url": "h", "published_at": null, "body": " notes ",
             "assets": [{"name": "A-v1.0.zip", "size": 1, "browser_download_url": format!("{base}/c")}]}
        ]);
        Mock::given(path("/repos/o/A/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let a = AddonRef::new(SourceId::Github, "o/A");
        let pick = |c| {
            let (gh, a) = (&gh, &a);
            async move { gh.resolve(a, &Flavor::Forever, c).await.unwrap() }
        };
        let stable = pick(Channel::Stable).await;
        assert_eq!(stable.version, "v1.0.0");
        assert_eq!(stable.changelog.as_deref(), Some("notes"));
        assert_eq!(pick(Channel::Beta).await.version, "v1.1.0-beta2");
        assert_eq!(pick(Channel::Alpha).await.channel, Channel::Alpha);

        // Notes from the newest accepted release down to (not including) the installed one.
        let notes = gh
            .changes(&a, Channel::Beta, "v1.0.0")
            .await
            .unwrap()
            .unwrap();
        let versions: Vec<_> = notes.iter().map(|c| c.version.as_str()).collect();
        assert_eq!(versions, ["v1.1.0-beta2"]);
        let stable = gh
            .changes(&a, Channel::Stable, "v0.9")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stable[0].notes.as_deref(), Some("notes"));
    }

    #[test]
    fn compares_kodo_versions() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.10.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-rc1", "0.1.1"));
        assert!(
            is_newer("v0.1.0", "0.1.0-rc.1"),
            "the final release beats its rc"
        );
        assert!(!is_newer("v0.1.0-rc.2", "0.1.0"));
    }

    #[test]
    fn falls_back_to_zip_names() {
        let asset = |n: &str| GhAsset {
            name: n.into(),
            size: 1,
            browser_download_url: n.into(),
        };
        let multi = [
            asset("Addon-v1.zip"),
            asset("Addon-v1-classic.zip"),
            asset("Addon-v1-camelot.zip"),
            asset("Addon-v1-nolib.zip"),
        ];
        let name = |f: &Flavor, a: &[GhAsset]| pick_by_name(a, f).map(|a| a.name.clone());
        assert_eq!(
            name(&Flavor::Forever, &multi).as_deref(),
            Some("Addon-v1-camelot.zip")
        );
        assert_eq!(
            name(&Flavor::Retail, &multi).as_deref(),
            Some("Addon-v1.zip")
        );
        assert_eq!(
            name(&Flavor::ClassicEra, &multi).as_deref(),
            Some("Addon-v1-classic.zip")
        );
        let single = [asset("Addon-v1-mists.zip"), asset("README.md")];
        assert_eq!(
            name(&Flavor::Forever, &single).as_deref(),
            Some("Addon-v1-mists.zip")
        );
        assert_eq!(name(&Flavor::Forever, &[asset("notes.txt")]), None);
    }

    #[tokio::test]
    async fn searches_and_suggests_from_catalogue() {
        let (_server, gh, _dir) = setup().await;
        let q = |t: &str| Query {
            text: t.into(),
            limit: 10,
            ..Query::default()
        };
        let hits = gh.search(&q("bags"), &Flavor::Forever).await.unwrap();
        let ids: Vec<_> = hits.iter().map(|h| h.addon.id.as_str()).collect();
        assert_eq!(ids, ["Cidan/BetterBags"]);
        assert!(
            gh.search(&q("tosui"), &Flavor::Forever)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            gh.search(&q("tosui"), &Flavor::Retail).await.unwrap().len(),
            1
        );

        // Empty text browses everything the flavor supports, in the requested order.
        let browse = Query {
            sort: Sort::Updated,
            limit: 100,
            ..Query::default()
        };
        let all = gh.search(&browse, &Flavor::Forever).await.unwrap();
        assert!(all.len() > 1);
        assert!(all.iter().all(|h| h.addon.id != "0kamiyasha/TOSUI"));
        assert!(all.windows(2).all(|w| w[0].updated >= w[1].updated));
        assert!(
            all.iter()
                .all(|h| h.updated.as_ref().is_some_and(|d| d.len() == 10))
        );

        let folder = |name: &str, title: Option<&str>| LocalAddon {
            folder: name.into(),
            toc_file: None,
            toc: title.map(|t| Toc {
                title: Some(t.into()),
                ..Toc::default()
            }),
            out_of_date: false,
        };
        let local = [
            folder("BetterBags", None),
            folder("BigWigs_Core", Some("Big Wigs")),
            folder("Sheepdog", None),
            folder("MyOwnThing", Some("Mine")),
        ];
        let found = gh.suggest(&local, &Flavor::Forever).await.unwrap();
        let pairs: Vec<_> = found
            .iter()
            .map(|m| (m.folders[0].as_str(), m.addon.id.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("BetterBags", "Cidan/BetterBags"),
                ("BigWigs_Core", "BigWigsMods/BigWigs")
            ]
        );

        // Forks sharing a name: the most downloaded wins.
        let other = gh
            .suggest(&local[2..3], &Flavor::Other("anniversary".into()))
            .await
            .unwrap();
        assert_eq!(other[0].addon.id, "2072/sheepdog");
    }
}
