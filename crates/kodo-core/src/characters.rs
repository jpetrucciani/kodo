//! Per-character addon switches: `WTF/Account/<account>/<realm>/<character>/AddOns.txt`, one
//! `Folder: enabled` or `Folder: disabled` line per addon the character has seen. Folders it
//! doesn't list load by default. WoW rewrites the file when the character logs out, so kodo
//! only writes it with the game closed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

pub const FILE: &str = "AddOns.txt";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Character {
    pub account: String,
    /// The realm folder as WoW names it (Forever uses a number).
    pub realm: String,
    /// The character folder, e.g. `Jacobi-Frostwhisper`.
    pub name: String,
}

impl Character {
    /// `account/realm/name`, unique per client; how commands pick a character.
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.account, self.realm, self.name)
    }

    /// Its folder under `WTF`.
    pub fn rel_dir(&self) -> PathBuf {
        Path::new("Account")
            .join(&self.account)
            .join(&self.realm)
            .join(&self.name)
    }

    /// Path of its AddOns.txt under `WTF`.
    pub fn rel_file(&self) -> PathBuf {
        self.rel_dir().join(FILE)
    }

    /// Whether `query` names this character: its key, or its folder name.
    pub fn is(&self, query: &str) -> bool {
        self.key().eq_ignore_ascii_case(query) || self.name.eq_ignore_ascii_case(query)
    }
}

/// Characters under `wtf`: folders at `Account/<account>/<realm>/<character>` that look like a
/// character (they hold an AddOns.txt, SavedVariables or a config cache).
pub fn list(wtf: &Path) -> Vec<Character> {
    let mut out = Vec::new();
    for account in subdirs(&wtf.join("Account")) {
        for realm in subdirs(&wtf.join("Account").join(&account)) {
            if realm.eq_ignore_ascii_case("SavedVariables") {
                continue;
            }
            let realm_dir = wtf.join("Account").join(&account).join(&realm);
            for name in subdirs(&realm_dir) {
                let dir = realm_dir.join(&name);
                let looks_like_one = [FILE, "SavedVariables", "config-cache.wtf"]
                    .iter()
                    .any(|f| dir.join(f).exists());
                if looks_like_one {
                    out.push(Character {
                        account: account.clone(),
                        realm: realm.clone(),
                        name,
                    });
                }
            }
        }
    }
    out.sort_by_key(|c| c.key().to_lowercase());
    out
}

fn subdirs(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// When the character last logged out, in Unix seconds: WoW rewrites the caches in its folder
/// (`config-cache.wtf` and friends) then. `None` before its first logout.
pub fn last_played(wtf: &Path, c: &Character) -> Option<u64> {
    std::fs::read_dir(wtf.join(c.rel_dir()))
        .ok()?
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
            meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()
        })
        .map(|d| d.as_secs())
        .max()
}

/// Addon folders (lowercased) that keep settings just for this character, from its own
/// `SavedVariables` folder.
pub fn saved(wtf: &Path, c: &Character) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(wtf.join(c.rel_dir()).join("SavedVariables")) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".lua")?;
            Some(stem.to_lowercase())
        })
        .collect()
}

/// The client's region (`SET portal "US"` in `Config.wtf`).
pub fn region(wtf: &Path) -> Option<String> {
    let text = std::fs::read_to_string(wtf.join("Config.wtf")).ok()?;
    text.lines().find_map(|l| {
        let value = l
            .trim()
            .strip_prefix("SET portal ")?
            .trim()
            .trim_matches('"');
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// Folder → enabled, as the file lists them. Unreadable or missing means nothing listed.
pub fn read(wtf: &Path, c: &Character) -> BTreeMap<String, bool> {
    parse(&std::fs::read_to_string(wtf.join(c.rel_file())).unwrap_or_default())
}

pub fn parse(text: &str) -> BTreeMap<String, bool> {
    text.lines()
        .filter_map(|line| {
            let (folder, state) = line.split_once(':')?;
            let folder = folder.trim();
            (!folder.is_empty()).then(|| {
                (
                    folder.to_owned(),
                    !state.trim().eq_ignore_ascii_case("disabled"),
                )
            })
        })
        .collect()
}

/// Whether the folder loads for the character: listed state, else on.
pub fn is_enabled(states: &BTreeMap<String, bool>, folder: &str) -> bool {
    states
        .iter()
        .find(|(f, _)| f.eq_ignore_ascii_case(folder))
        .is_none_or(|(_, on)| *on)
}

/// The file with `changes` applied: listed folders change in place, new ones are appended.
/// Keeps the file's line endings.
pub fn render(existing: &str, changes: &[(String, bool)]) -> String {
    let eol = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let word = |on: bool| if on { "enabled" } else { "disabled" };
    let mut pending: Vec<&(String, bool)> = changes.iter().collect();
    let mut lines: Vec<String> = Vec::new();
    for line in existing.lines() {
        let folder = line.split_once(':').map(|(f, _)| f.trim());
        let hit = folder.and_then(|f| pending.iter().position(|(c, _)| c.eq_ignore_ascii_case(f)));
        match (folder, hit) {
            (Some(f), Some(i)) => {
                lines.push(format!("{f}: {}", word(pending[i].1)));
                pending.remove(i);
            }
            _ if line.trim().is_empty() => {}
            _ => lines.push(line.to_owned()),
        }
    }
    lines.extend(pending.iter().map(|(f, on)| format!("{f}: {}", word(*on))));
    let mut text = lines.join(eol);
    text.push_str(eol);
    text
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::characters::{is_enabled, list, parse, region, render, saved};

    #[test]
    fn reads_and_edits_addons_txt() {
        // As a Forever client writes it.
        let text = "Blizzard_ObjectiveTracker: enabled\r\nDetails: disabled\r\n";
        let states = parse(text);
        assert!(is_enabled(&states, "blizzard_objectivetracker"));
        assert!(!is_enabled(&states, "Details"));
        assert!(is_enabled(&states, "NotListed"), "unlisted folders load");

        let out = render(text, &[("details".into(), true), ("Bags".into(), false)]);
        assert_eq!(
            out,
            "Blizzard_ObjectiveTracker: enabled\r\nDetails: enabled\r\nBags: disabled\r\n"
        );
        assert_eq!(render("", &[("A".into(), true)]), "A: enabled\n");
        assert_eq!(parse("junk\n: enabled\n"), BTreeMap::new());
    }

    #[test]
    fn finds_character_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let wtf = tmp.path();
        let acct = wtf.join("Account/123#1");
        std::fs::create_dir_all(acct.join("SavedVariables")).unwrap();
        std::fs::create_dir_all(acct.join("91/Jacobi-Frostwhisper")).unwrap();
        std::fs::write(acct.join("91/Jacobi-Frostwhisper/AddOns.txt"), "").unwrap();
        std::fs::create_dir_all(acct.join("91/Empty")).unwrap();
        let found = list(wtf);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key(), "123#1/91/Jacobi-Frostwhisper");
        assert!(found[0].is("jacobi-frostwhisper"));

        let sv = acct.join("91/Jacobi-Frostwhisper/SavedVariables");
        std::fs::create_dir_all(&sv).unwrap();
        std::fs::write(sv.join("PocketChange.lua"), "").unwrap();
        std::fs::write(sv.join("PocketChange.lua.bak"), "").unwrap();
        assert_eq!(saved(wtf, &found[0]), ["pocketchange".to_owned()].into());
        std::fs::write(
            wtf.join("Config.wtf"),
            "SET locale \"enUS\"\r\nSET portal \"US\"\r\n",
        )
        .unwrap();
        assert_eq!(region(wtf).as_deref(), Some("US"));
    }
}
