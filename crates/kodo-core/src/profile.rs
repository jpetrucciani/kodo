//! Profiles: named sets of addons per client. One is always active and mirrors what's
//! installed; switching stashes the addons the new profile doesn't list and brings back (or
//! installs) the ones it does.
//!
//! A profile travels as a code, `kodo1-` plus unpadded base64url of a small text payload:
//! the name, the flavor id, then one tab-separated `ref  channel  pinned-version  name` line
//! per addon (empty fields allowed, trailing ones dropped), followed by a 4-byte FNV-1a
//! checksum so a truncated paste is rejected instead of importing half a profile.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::addon::{AddonRef, Channel};
use crate::flavor::Flavor;
use crate::lockfile::LockEntry;
use crate::{Error, Result, io_err};

const CODE_PREFIX: &str = "kodo1-";
/// Name of the profile a client starts on.
pub const DEFAULT_PROFILE: &str = "Default";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileAddon {
    pub id: AddonRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub channel: Channel,
    /// Pinned version; `None` follows the channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl ProfileAddon {
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.id.to_string())
    }
}

impl From<&LockEntry> for ProfileAddon {
    fn from(e: &LockEntry) -> Self {
        Self {
            id: e.id.clone(),
            name: Some(e.display_name().to_owned()),
            channel: e.channel,
            version: e.pinned.then(|| e.version.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default, rename = "addon")]
    pub addons: Vec<ProfileAddon>,
}

impl Profile {
    /// Installed addons, or just those in `only`.
    pub fn from_entries(name: &str, entries: &[LockEntry], only: Option<&[AddonRef]>) -> Self {
        Self {
            name: name.trim().to_owned(),
            addons: entries
                .iter()
                .filter(|e| only.is_none_or(|o| o.contains(&e.id)))
                .map(ProfileAddon::from)
                .collect(),
        }
    }

    pub fn contains(&self, id: &AddonRef) -> bool {
        self.addons.iter().any(|a| &a.id == id)
    }

    /// Shareable code for this profile on `flavor`.
    pub fn to_code(&self, flavor: &Flavor) -> String {
        let mut text = format!("{}\n{}", one_line(&self.name), flavor.id());
        for a in &self.addons {
            let channel = match a.channel {
                Channel::Stable => String::new(),
                c => c.as_str().to_owned(),
            };
            let fields = [
                a.id.to_string(),
                channel,
                a.version.as_deref().map(one_line).unwrap_or_default(),
                a.name.as_deref().map(one_line).unwrap_or_default(),
            ];
            let used = fields
                .iter()
                .rposition(|f| !f.is_empty())
                .map_or(1, |i| i + 1);
            text.push('\n');
            text.push_str(&fields[..used].join("\t"));
        }
        let mut bytes = text.into_bytes();
        let sum = checksum(&bytes);
        bytes.extend_from_slice(&sum);
        format!("{CODE_PREFIX}{}", base64url::encode(&bytes))
    }

    /// Decode a code (whitespace anywhere is ignored, so wrapped pastes work).
    pub fn from_code(code: &str) -> Result<(Self, Flavor)> {
        let bad = |why: &str| Error::BadRef(format!("not a kodo profile code: {why}"));
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let body = code
            .strip_prefix(CODE_PREFIX)
            .ok_or_else(|| bad("it should start with kodo1-"))?;
        let damaged = || bad("it is damaged or incomplete");
        let mut bytes = base64url::decode(body).ok_or_else(damaged)?;
        let sum = bytes.split_off(bytes.len().checked_sub(4).ok_or_else(damaged)?);
        if sum != checksum(&bytes) {
            return Err(damaged());
        }
        let text = String::from_utf8(bytes).map_err(|_| damaged())?;
        let mut lines = text.lines();
        let name = lines.next().unwrap_or_default().trim().to_owned();
        let flavor = Flavor::from_id(lines.next().unwrap_or_default().trim());
        let mut addons = Vec::new();
        for line in lines.filter(|l| !l.trim().is_empty()) {
            let mut f = line.split('\t').map(str::trim);
            let id: AddonRef = f.next().unwrap_or_default().parse()?;
            let channel = match f.next().filter(|s| !s.is_empty()) {
                Some(c) => c.parse()?,
                None => Channel::Stable,
            };
            let opt = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(str::to_owned);
            let version = opt(f.next());
            let name = opt(f.next());
            addons.push(ProfileAddon {
                id,
                name,
                channel,
                version,
            });
        }
        if name.is_empty() {
            return Err(bad("it has no name"));
        }
        Ok((Self { name, addons }, flavor))
    }
}

fn checksum(bytes: &[u8]) -> [u8; 4] {
    // Truncation keeps the low 32 bits.
    (crate::fnv1a(bytes) as u32).to_le_bytes()
}

fn one_line(s: &str) -> String {
    s.replace(['\n', '\r', '\t'], " ")
}

/// Every profile of one client, and which one is active.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profiles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    #[serde(default, rename = "profile")]
    pub profiles: Vec<Profile>,
}

impl Profiles {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| Error::BadFile {
                path: path.to_path_buf(),
                message: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(io_err("read", path)(e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| Error::BadFile {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        crate::config::write_atomic(path, text.as_bytes())
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name.trim()))
    }

    /// Replace the profile with the same name, or add it.
    pub fn upsert(&mut self, profile: Profile) {
        match self
            .profiles
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(&profile.name))
        {
            Some(p) => *p = profile,
            None => self.profiles.push(profile),
        }
    }

    pub fn remove(&mut self, name: &str) -> Option<Profile> {
        let i = self
            .profiles
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(name.trim()))?;
        if self.is_active(name) {
            self.active = None;
        }
        Some(self.profiles.remove(i))
    }

    /// Make sure a profile is active (creating "Default" if none is) and that it lists exactly
    /// `entries`, the installed addons. Returns whether anything changed.
    pub fn sync_active(&mut self, entries: &[LockEntry]) -> bool {
        let stored = self
            .active
            .as_deref()
            .and_then(|a| self.get(a))
            .map(|p| p.name.clone());
        let name = stored.unwrap_or_else(|| self.unique_name(DEFAULT_PROFILE));
        let live = Profile::from_entries(&name, entries, None);
        let changed =
            self.active.as_deref() != Some(name.as_str()) || self.get(&name) != Some(&live);
        self.active = Some(name);
        self.upsert(live);
        changed
    }

    pub fn is_active(&self, name: &str) -> bool {
        self.active
            .as_deref()
            .is_some_and(|a| a.eq_ignore_ascii_case(name.trim()))
    }

    /// `base`, or `base (2)`, `base (3)`… if taken.
    pub fn unique_name(&self, base: &str) -> String {
        let base = base.trim();
        (1..)
            .map(|n| {
                if n == 1 {
                    base.to_owned()
                } else {
                    format!("{base} ({n})")
                }
            })
            .find(|n| self.get(n).is_none())
            .unwrap_or_else(|| base.to_owned())
    }
}

/// RFC 4648 base64url without padding for profile codes, plus a lenient decoder for standard
/// base64 (WowUp's export strings).
pub(crate) mod base64url {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
            for i in 0..=chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            }
        }
        out
    }

    pub fn decode(text: &str) -> Option<Vec<u8>> {
        let digits: Vec<u32> = text
            .bytes()
            .map(|c| ALPHABET.iter().position(|&a| a == c).map(|p| p as u32))
            .collect::<Option<_>>()?;
        from_digits(&digits)
    }

    /// Standard base64 too: `+`, `/`, padding and line breaks.
    pub fn decode_lenient(text: &str) -> Option<Vec<u8>> {
        let digits: Vec<u32> = text
            .bytes()
            .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
            .map(|c| match c {
                b'+' => Some(62),
                b'/' => Some(63),
                _ => ALPHABET.iter().position(|&a| a == c).map(|p| p as u32),
            })
            .collect::<Option<_>>()?;
        from_digits(&digits)
    }

    fn from_digits(digits: &[u32]) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(digits.len() * 3 / 4);
        for chunk in digits.chunks(4) {
            if chunk.len() == 1 {
                return None;
            }
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, &d)| n | d << (18 - 6 * i));
            for i in 0..chunk.len() - 1 {
                out.push((n >> (16 - 8 * i)) as u8);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::addon::{AddonRef, Channel, SourceId};
    use crate::flavor::Flavor;
    use crate::lockfile::LockEntry;
    use crate::profile::{Profile, ProfileAddon, Profiles, base64url};

    #[test]
    fn base64url_round_trips() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            assert_eq!(
                base64url::decode(&base64url::encode(s.as_bytes())).unwrap(),
                s.as_bytes()
            );
        }
        assert_eq!(base64url::encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url::encode(b"\xfb\xff"), "-_8");
        assert!(base64url::decode("a").is_none());
        assert!(base64url::decode("ab+c").is_none());
    }

    #[test]
    fn code_round_trips_and_rejects_junk() {
        let p = Profile {
            name: "Raid\tnight".into(),
            addons: vec![
                ProfileAddon {
                    id: AddonRef::new(SourceId::Github, "Cidan/BetterBags"),
                    name: Some("BetterBags".into()),
                    channel: Channel::Stable,
                    version: None,
                },
                ProfileAddon {
                    id: AddonRef::new(SourceId::Wowi, "3896"),
                    name: None,
                    channel: Channel::Beta,
                    version: Some("v1.54.07".into()),
                },
            ],
        };
        let code = p.to_code(&Flavor::Forever);
        assert!(code.starts_with("kodo1-"));
        // Chat clients wrap long lines; whitespace is ignored.
        let wrapped = format!(" {}\n{} ", &code[..20], &code[20..]);
        let (back, flavor) = Profile::from_code(&wrapped).unwrap();
        assert_eq!(flavor, Flavor::Forever);
        assert_eq!(back.name, "Raid night");
        assert_eq!(back.addons, p.addons);

        assert!(Profile::from_code("hello").is_err());
        assert!(Profile::from_code(&code[..code.len() - 7]).is_err());
        let flipped = code
            .replacen("kodo1-A", "kodo1-B", 1)
            .replacen("kodo1-a", "kodo1-b", 1);
        if flipped != code {
            assert!(Profile::from_code(&flipped).is_err());
        }
    }

    #[test]
    fn names_are_unique_and_active_clears_on_remove() {
        let mut set = Profiles::default();
        for _ in 0..2 {
            let name = set.unique_name("Default");
            set.upsert(Profile {
                name,
                addons: Vec::new(),
            });
        }
        assert_eq!(set.profiles[1].name, "Default (2)");
        set.active = Some("default".into());
        set.remove("DEFAULT");
        assert_eq!(set.active, None);
        assert_eq!(set.profiles.len(), 1);
    }

    #[test]
    fn active_profile_mirrors_installed_addons() {
        let id = AddonRef::new(SourceId::Github, "Cidan/BetterBags");
        let entries = vec![LockEntry::new(id.clone(), "v1", vec!["BetterBags".into()])];
        let mut set = Profiles::default();
        assert!(set.sync_active(&entries));
        assert_eq!(set.active.as_deref(), Some("Default"));
        assert!(set.get("default").is_some_and(|p| p.contains(&id)));
        assert!(
            !set.sync_active(&entries),
            "nothing changed the second time"
        );
        assert!(set.sync_active(&[]));
        assert!(set.get("Default").is_some_and(|p| p.addons.is_empty()));
        assert_eq!(set.profiles.len(), 1);
    }
}
