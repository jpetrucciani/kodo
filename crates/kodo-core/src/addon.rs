//! Source-independent addon types shared by every source adapter and frontend.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceId {
    Github,
    Wowi,
    Curseforge,
    Wago,
}

impl SourceId {
    pub const ALL: [Self; 4] = [Self::Github, Self::Wowi, Self::Curseforge, Self::Wago];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Wowi => "wowi",
            Self::Curseforge => "curseforge",
            Self::Wago => "wago",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Wowi => "WoWInterface",
            Self::Curseforge => "CurseForge",
            Self::Wago => "Wago",
        }
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SourceId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        match s.to_ascii_lowercase().as_str() {
            "github" | "gh" => Ok(Self::Github),
            "wowi" | "wowinterface" => Ok(Self::Wowi),
            "curseforge" | "cf" => Ok(Self::Curseforge),
            "wago" => Ok(Self::Wago),
            _ => Err(Error::BadRef(format!("unknown source {s:?}"))),
        }
    }
}

/// Release channel, ordered from most to least stable.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
    Alpha,
}

impl Channel {
    /// Whether a release on `release` may be installed by someone following `self`.
    pub fn accepts(self, release: Self) -> bool {
        release <= self
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Alpha => "alpha",
        }
    }
}

impl FromStr for Channel {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        match s.to_ascii_lowercase().as_str() {
            "stable" | "release" => Ok(Self::Stable),
            "beta" => Ok(Self::Beta),
            "alpha" => Ok(Self::Alpha),
            _ => Err(Error::BadRef(format!("unknown channel {s:?}"))),
        }
    }
}

/// A source plus that source's id for an addon: `github:owner/repo`, `wowi:12345`, `curseforge:67890`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct AddonRef {
    pub source: SourceId,
    pub id: String,
}

impl AddonRef {
    pub fn new(source: SourceId, id: impl Into<String>) -> Self {
        Self {
            source,
            id: id.into(),
        }
    }

    /// Parse a project URL from a known site.
    fn from_url(url: &str) -> Option<Self> {
        let rest = url
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.");
        let mut parts = rest.split(['/', '?', '#']).filter(|p| !p.is_empty());
        match parts.next()?.to_ascii_lowercase().as_str() {
            "github.com" => {
                let owner = parts.next()?;
                let repo = parts.next()?.trim_end_matches(".git");
                Some(Self::new(SourceId::Github, format!("{owner}/{repo}")))
            }
            "wowinterface.com" => {
                // /downloads/info12345-Name.html or /downloads/download12345-Name
                let page = parts.nth(1)?;
                let digits: String = page
                    .trim_start_matches(|c: char| c.is_ascii_alphabetic())
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                (!digits.is_empty()).then(|| Self::new(SourceId::Wowi, digits))
            }
            _ => None,
        }
    }
}

impl fmt::Display for AddonRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source, self.id)
    }
}

impl FromStr for AddonRef {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        let s = s.trim();
        let bare_url = ["github.com/", "wowinterface.com/", "www."]
            .iter()
            .any(|p| s.to_ascii_lowercase().starts_with(p));
        if s.contains("://") || bare_url {
            return Self::from_url(s).ok_or_else(|| {
                Error::BadRef(format!(
                    "{s:?} is not a GitHub or WoWInterface project URL (use cf:<project id> for CurseForge)"
                ))
            });
        }
        let (source, id) = s.split_once(':').ok_or_else(|| {
            Error::BadRef(format!(
                "{s:?} should look like github:owner/repo, wowi:12345 or cf:67890"
            ))
        })?;
        let source: SourceId = source.parse()?;
        let id = id.trim();
        let valid = match source {
            SourceId::Github => id.split('/').count() == 2 && id.split('/').all(|p| !p.is_empty()),
            SourceId::Wowi | SourceId::Curseforge => {
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
            }
            SourceId::Wago => !id.is_empty(),
        };
        if !valid {
            return Err(Error::BadRef(format!("{id:?} is not a valid {source} id")));
        }
        Ok(Self::new(source, id))
    }
}

impl From<AddonRef> for String {
    fn from(r: AddonRef) -> Self {
        r.to_string()
    }
}

impl TryFrom<String> for AddonRef {
    type Error = Error;
    fn try_from(s: String) -> Result<Self, Error> {
        s.parse()
    }
}

/// Result order for search and browse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Sort {
    /// Closest name match first, then downloads. Without search text this is `Downloads`.
    #[default]
    Relevance,
    Downloads,
    Updated,
    Name,
}

impl Sort {
    pub const ALL: [Self; 4] = [Self::Relevance, Self::Downloads, Self::Updated, Self::Name];

    pub fn label(self) -> &'static str {
        match self {
            Self::Relevance => "Best match",
            Self::Downloads => "Most downloaded",
            Self::Updated => "Recently updated",
            Self::Name => "Name",
        }
    }
}

impl fmt::Display for Sort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for Sort {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        match s.to_ascii_lowercase().as_str() {
            "relevance" => Ok(Self::Relevance),
            "downloads" => Ok(Self::Downloads),
            "updated" => Ok(Self::Updated),
            "name" => Ok(Self::Name),
            _ => Err(Error::BadRef(format!(
                "unknown sort {s:?} (relevance, downloads, updated, name)"
            ))),
        }
    }
}

/// Search text (empty browses everything), result cap and order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub text: String,
    pub limit: usize,
    pub sort: Sort,
}

impl Query {
    /// Whether `haystacks` contain the search text (always true when browsing).
    pub fn matches(&self, haystacks: &[&str]) -> bool {
        let q = self.text.trim().to_lowercase();
        q.is_empty() || haystacks.iter().any(|h| h.to_lowercase().contains(&q))
    }

    /// Order hits by `sort` and keep the first `limit` (0 means 25).
    pub fn rank(&self, hits: &mut Vec<RemoteAddon>) {
        let q = self.text.trim().to_lowercase();
        let downloads = |h: &RemoteAddon| std::cmp::Reverse(h.downloads.unwrap_or(0));
        match self.sort {
            Sort::Relevance if !q.is_empty() => hits.sort_by_cached_key(|h| {
                let name = h.name.to_lowercase();
                let closeness = if name == q {
                    0
                } else if name.starts_with(&q) {
                    1
                } else if name.contains(&q) {
                    2
                } else {
                    3
                };
                (closeness, downloads(h))
            }),
            Sort::Relevance | Sort::Downloads => hits.sort_by_key(downloads),
            Sort::Updated => hits.sort_by(|a, b| b.updated.cmp(&a.updated)),
            Sort::Name => hits.sort_by_cached_key(|h| h.name.to_lowercase()),
        }
        hits.truncate(if self.limit == 0 { 25 } else { self.limit });
    }
}

/// A search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteAddon {
    pub addon: AddonRef,
    pub name: String,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub downloads: Option<u64>,
    pub page_url: Option<String>,
    pub version: Option<String>,
    /// Last update as a UTC `YYYY-MM-DD`, so it sorts as text.
    pub updated: Option<String>,
    /// Game tracks the source says it has builds for (`forever`, `retail`, …), if it says.
    pub tracks: Vec<String>,
}

/// One concrete, downloadable build of an addon for a flavor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Release {
    pub addon: AddonRef,
    pub name: String,
    pub version: String,
    pub channel: Channel,
    /// `None` when the author disallows third-party downloads; send the user to `page_url`.
    pub download_url: Option<String>,
    pub page_url: Option<String>,
    pub size: Option<u64>,
    pub changelog: Option<String>,
    pub published_at: Option<String>,
}

/// One release's notes, for reading what changed before updating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub version: String,
    pub published_at: Option<String>,
    /// Markdown as the author wrote it.
    pub notes: Option<String>,
}

impl From<&Release> for Change {
    fn from(r: &Release) -> Self {
        Self {
            version: r.version.clone(),
            published_at: r.published_at.clone(),
            notes: r.changelog.clone(),
        }
    }
}

/// A source's claim that some installed folders are one of its addons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Match {
    pub folders: Vec<String>,
    pub addon: AddonRef,
    pub version: Option<String>,
}

#[cfg(test)]
mod tests {
    use crate::addon::{AddonRef, Channel, SourceId};

    #[test]
    fn parses_refs_and_urls() {
        let gh = AddonRef::new(SourceId::Github, "Cidan/BetterBags");
        assert_eq!("github:Cidan/BetterBags".parse::<AddonRef>().unwrap(), gh);
        assert_eq!("gh:Cidan/BetterBags".parse::<AddonRef>().unwrap(), gh);
        assert_eq!(
            "https://github.com/Cidan/BetterBags/releases"
                .parse::<AddonRef>()
                .unwrap(),
            gh
        );
        assert_eq!(
            "https://www.wowinterface.com/downloads/info3896-Atlas.html"
                .parse::<AddonRef>()
                .unwrap(),
            AddonRef::new(SourceId::Wowi, "3896")
        );
        assert_eq!(
            "cf:942432".parse::<AddonRef>().unwrap().to_string(),
            "curseforge:942432"
        );
        for bad in [
            "BetterBags",
            "github:Cidan",
            "wowi:abc",
            "https://example.com/x/y",
            "nope:1",
        ] {
            assert!(bad.parse::<AddonRef>().is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn channel_acceptance() {
        assert!(Channel::Beta.accepts(Channel::Stable));
        assert!(!Channel::Stable.accepts(Channel::Beta));
    }
}
