//! A per-client log of every change kodo made, newest last, as JSON lines. Each event can carry
//! how to undo it. Logging is best effort: a full disk shouldn't fail an install.

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::addon::AddonRef;

/// Events kept; the file is trimmed to this once it holds twice as many.
pub const KEEP: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Undo {
    /// Restore the backup taken before the change (also brings back a removed addon).
    Rollback {
        addon: AddonRef,
    },
    /// Remove a fresh install.
    Remove {
        addon: AddonRef,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    /// Switch back to the profile that was active.
    Switch {
        profile: String,
    },
    /// Put saved settings back from this snapshot.
    RestoreWtf {
        snapshot: String,
    },
    /// Set these per-character switches back.
    CharacterAddons {
        character: String,
        states: Vec<(String, bool)>,
    },
}

/// What sort of change an event was, for grouping and coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Install,
    Update,
    Remove,
    Rollback,
    Switch,
    Disable,
    Enable,
    /// Saved settings put back from a snapshot.
    Restore,
    /// Per-character addon switches.
    Characters,
    /// Logged before kinds existed.
    #[default]
    #[serde(other)]
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Install => "Install",
            Self::Update => "Update",
            Self::Remove => "Remove",
            Self::Rollback => "Rollback",
            Self::Switch => "Profile",
            Self::Disable => "Disable",
            Self::Enable => "Enable",
            Self::Restore => "Settings",
            Self::Characters => "Characters",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Unix seconds.
    pub at: u64,
    #[serde(default)]
    pub kind: Kind,
    pub what: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<Undo>,
}

impl Event {
    pub fn now(kind: Kind, what: impl Into<String>, undo: Option<Undo>) -> Self {
        Self {
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            kind,
            what: what.into(),
            undo,
        }
    }
}

/// Append an event, trimming old ones now and then. Errors are dropped (see module docs).
pub fn record(path: &Path, event: &Event) {
    let Ok(line) = serde_json::to_string(event) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if appended.is_err() {
        return;
    }
    let all = read_all(path);
    if all.len() > KEEP * 2 {
        let text: String = all[all.len() - KEEP..]
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .map(|l| l + "\n")
            .collect();
        let _ = crate::config::write_atomic(path, text.as_bytes());
    }
}

/// Events, newest first; lines that don't parse are skipped.
pub fn list(path: &Path) -> Vec<Event> {
    let mut all = read_all(path);
    all.reverse();
    all
}

fn read_all(path: &Path) -> Vec<Event> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::history::{Event, KEEP, Kind, Undo, list, record};

    #[test]
    fn logs_newest_first_and_trims() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("h/history.jsonl");
        let undo = Undo::Rollback {
            addon: "github:a/b".parse().unwrap(),
        };
        record(
            &path,
            &Event::now(Kind::Update, "updated b", Some(undo.clone())),
        );
        record(&path, &Event::now(Kind::Disable, "disabled c", None));
        let events = list(&path);
        assert_eq!(events[0].what, "disabled c");
        assert_eq!(events[1].undo, Some(undo));
        assert_eq!(events[1].kind, Kind::Update);
        // Lines from before kinds existed still read.
        let old: Event = serde_json::from_str(r#"{"at":1,"what":"x"}"#).unwrap();
        assert_eq!(old.kind, Kind::Other);

        for i in 0..KEEP * 2 {
            record(&path, &Event::now(Kind::Other, format!("e{i}"), None));
        }
        let events = list(&path);
        assert!(events.len() <= KEEP * 2 && events.len() >= KEEP);
        assert_eq!(events[0].what, format!("e{}", KEEP * 2 - 1));
    }
}
