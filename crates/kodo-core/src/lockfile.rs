//! `kodo.lock.toml`: which addons kodo installed into a client, and which folders each owns.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::addon::{AddonRef, Channel, SourceId};
use crate::flavor::Flavor;
use crate::{Error, Result, io_err};

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    pub version: u32,
    pub client: Flavor,
    /// The client's AddOns folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, rename = "addon")]
    pub addons: Vec<LockEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    pub id: AddonRef,
    pub source: SourceId,
    /// The source-specific id (`owner/repo`, `12345`).
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub version: String,
    #[serde(default)]
    pub channel: Channel,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub folders: Vec<String>,
    /// RFC 3339 UTC.
    pub installed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl LockEntry {
    pub fn new(id: AddonRef, version: impl Into<String>, folders: Vec<String>) -> Self {
        Self {
            source: id.source,
            reference: id.id.clone(),
            id,
            name: None,
            version: version.into(),
            channel: Channel::Stable,
            pinned: false,
            folders,
            installed_at: now_rfc3339(),
            asset_url: None,
            sha256: None,
        }
    }

    /// Name to show: the addon name if known; for an unnamed bundle of folders, the repo name;
    /// else its first folder, else the ref.
    pub fn display_name(&self) -> &str {
        let repo = (self.folders.len() > 1)
            .then(|| self.reference.rsplit_once('/').map(|(_, r)| r))
            .flatten();
        self.name
            .as_deref()
            .or(repo)
            .or(self.folders.first().map(String::as_str))
            .unwrap_or(&self.reference)
    }

    pub fn owns(&self, folder: &str) -> bool {
        self.folders.iter().any(|f| f.eq_ignore_ascii_case(folder))
    }

    /// Whether a user-typed name refers to this entry (ref, id, name or any folder).
    pub fn answers_to(&self, query: &str) -> bool {
        let q = query.trim();
        self.id.to_string().eq_ignore_ascii_case(q)
            || self.reference.eq_ignore_ascii_case(q)
            || self
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(q))
            || self.owns(q)
    }
}

impl Lockfile {
    pub fn new(client: Flavor, path: PathBuf) -> Self {
        Self {
            version: VERSION,
            client,
            path: Some(path),
            addons: Vec::new(),
        }
    }

    /// Load a lockfile, or start an empty one if it doesn't exist yet.
    pub fn load_or_new(path: &Path, client: Flavor, addons_dir: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::new(client, addons_dir.to_path_buf()))
            }
            Err(e) => Err(io_err("read", path)(e)),
        }
    }

    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let lock: Self = toml::from_str(text).map_err(|e| Error::BadFile {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        if lock.version != VERSION {
            return Err(Error::BadFile {
                path: path.to_path_buf(),
                message: format!("unsupported lockfile version {}", lock.version),
            });
        }
        Ok(lock)
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| Error::BadFile {
            path: self.path.clone().unwrap_or_default(),
            message: e.to_string(),
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::config::write_atomic(path, self.to_toml()?.as_bytes())
    }

    pub fn get(&self, id: &AddonRef) -> Option<&LockEntry> {
        self.addons.iter().find(|e| e.id == *id)
    }

    pub fn owner_of(&self, folder: &str) -> Option<&LockEntry> {
        self.addons.iter().find(|e| e.owns(folder))
    }

    /// Find exactly one entry by ref, name or folder.
    pub fn find(&self, query: &str) -> Result<&LockEntry> {
        let hits: Vec<&LockEntry> = self.addons.iter().filter(|e| e.answers_to(query)).collect();
        match hits.as_slice() {
            [one] => Ok(one),
            [] => Err(Error::NotInstalled(query.to_owned())),
            many => Err(Error::Ambiguous(
                query.to_owned(),
                many.iter()
                    .map(|e| e.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
        }
    }

    /// Insert or replace the entry with the same id, keeping the list sorted for stable diffs.
    pub fn upsert(&mut self, entry: LockEntry) {
        self.addons.retain(|e| e.id != entry.id);
        self.addons.push(entry);
        self.addons
            .sort_by_cached_key(|e| e.display_name().to_lowercase());
    }

    pub fn remove(&mut self, id: &AddonRef) -> Option<LockEntry> {
        let i = self.addons.iter().position(|e| e.id == *id)?;
        Some(self.addons.remove(i))
    }
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now_rfc3339() -> String {
    rfc3339(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (UTC).
pub fn rfc3339(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::addon::AddonRef;
    use crate::flavor::Flavor;
    use crate::lockfile::{LockEntry, Lockfile, civil_from_days, rfc3339};

    #[test]
    fn round_trips_spec_shape() {
        let text = r#"
version = 1
client = "forever"
path = 'C:\Program Files (x86)\World of Warcraft\_forever_\Interface\AddOns'

[[addon]]
id = "github:Cidan/BetterBags"
source = "github"
ref = "Cidan/BetterBags"
version = "v0.9.2"
channel = "stable"
pinned = false
folders = ["BetterBags"]
installed_at = "2026-11-13T19:40:00Z"
asset_url = "https://github.com/Cidan/BetterBags/releases/download/v0.9.2/BetterBags-v0.9.2.zip"
sha256 = "abc"
"#;
        let lock = Lockfile::parse(text, Path::new("kodo.lock.toml")).unwrap();
        assert_eq!(lock.client, Flavor::Forever);
        let entry = lock.find("betterbags").unwrap();
        assert_eq!(
            entry.id,
            "github:Cidan/BetterBags".parse::<AddonRef>().unwrap()
        );
        assert_eq!(
            lock.owner_of("BETTERBAGS").map(|e| e.version.as_str()),
            Some("v0.9.2")
        );
        assert_eq!(
            Lockfile::parse(&lock.to_toml().unwrap(), Path::new("x")).unwrap(),
            lock
        );
        assert!(lock.find("nope").is_err());
    }

    #[test]
    fn find_rejects_ambiguous_names() {
        let mut lock = Lockfile::new(Flavor::Forever, "AddOns".into());
        let a = "github:a/Shared".parse().unwrap();
        let b = "wowi:1".parse().unwrap();
        lock.upsert(LockEntry::new(a, "1", vec!["Shared".into()]));
        lock.upsert(LockEntry::new(b, "1", vec!["Shared".into()]));
        assert!(lock.find("Shared").is_err());
        assert!(lock.find("wowi:1").is_ok());
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_405), (2025, 11, 13));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_794_416_784), "2026-11-11T17:06:24Z");
    }
}
