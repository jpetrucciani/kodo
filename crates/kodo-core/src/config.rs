//! `config.toml`, on-disk locations and secrets lookup.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::detect::{self, Client};
use crate::flavor::Flavor;
use crate::{Error, Result, io_err};

/// The `directories` organization: none, so Windows uses `%APPDATA%\kodo`. macOS keeps it, as
/// its folder is the bundle id `dev.cobi.kodo`; Linux ignores it.
const ORG: &str = if cfg!(target_os = "macos") {
    "cobi"
} else {
    ""
};
/// What Windows builds before 0.1.0 used.
const OLD_ORG: &str = "cobi";

/// The dir to use: `new`, after moving `old` there if only `old` exists. If the move fails (a
/// running older kodo holding a file, say) `old` stays in use so nothing looks lost. Emptied
/// parents (`cobi\kodo`, `cobi`) are removed; anything else in them stays.
fn move_dir(old: &Path, new: &Path) -> PathBuf {
    if old == new || !old.exists() || new.exists() {
        return new.to_path_buf();
    }
    let moved = new
        .parent()
        .is_none_or(|p| std::fs::create_dir_all(p).is_ok())
        && std::fs::rename(old, new).is_ok();
    if !moved {
        return old.to_path_buf();
    }
    for dir in old.ancestors().skip(1).take(2) {
        let _ = std::fs::remove_dir(dir);
    }
    new.to_path_buf()
}

/// Where kodo keeps its config, lockfiles and caches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Paths {
    /// Platform dirs, or a `kodo-data/` folder beside the binary in portable mode
    /// (`--portable`, or a `portable.txt` next to the executable). `KODO_CONFIG` overrides the
    /// config file location either way.
    pub fn resolve(portable: bool) -> Result<Self> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let portable_root = exe_dir
            .filter(|d| portable || d.join("portable.txt").is_file())
            .map(|d| d.join("kodo-data"));
        let mut paths = match portable_root {
            Some(root) => Self::under(&root),
            None => Self::platform()?,
        };
        if let Some(cfg) = std::env::var_os("KODO_CONFIG") {
            paths.config_file = PathBuf::from(cfg);
        }
        Ok(paths)
    }

    /// The OS's per-user dirs. Windows used to nest them under a `cobi` folder; anything still
    /// there moves over the first time, and stays in use if it can't be moved.
    fn platform() -> Result<Self> {
        let dirs = |org| {
            directories::ProjectDirs::from("dev", org, "kodo")
                .ok_or_else(|| Error::BadRef("no home directory found; use --portable".into()))
        };
        let new = dirs(ORG)?;
        let old = dirs(OLD_ORG)?;
        Ok(Self {
            config_file: move_dir(old.config_dir(), new.config_dir()).join("config.toml"),
            data_dir: move_dir(old.data_dir(), new.data_dir()),
            cache_dir: move_dir(old.cache_dir(), new.cache_dir()),
        })
    }

    /// Everything under one root, as in portable mode (also handy for tests).
    pub fn under(root: &Path) -> Self {
        Self {
            config_file: root.join("config.toml"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
        }
    }

    pub fn downloads(&self) -> PathBuf {
        self.cache_dir.join("downloads")
    }

    /// Per-client backups of replaced or removed addon folders.
    pub fn backups(&self, client: &Client) -> PathBuf {
        self.cache_dir.join("backups").join(client_key(client))
    }

    /// Source caches (HTTP ETags, catalogs).
    pub fn source_cache(&self) -> PathBuf {
        self.cache_dir.join("sources")
    }

    /// The client's saved profiles and which one is active.
    pub fn profiles(&self, client: &Client) -> PathBuf {
        self.data_dir.join(client_key(client)).join("profiles.toml")
    }

    /// Snapshots of the client's saved addon settings (see [`crate::wtf`]).
    pub fn wtf_backups(&self, client: &Client) -> PathBuf {
        self.data_dir.join(client_key(client)).join("wtf")
    }

    /// Everything kodo changed for the client (see [`crate::history`]).
    pub fn history(&self, client: &Client) -> PathBuf {
        self.data_dir.join(client_key(client)).join("history.jsonl")
    }

    /// Latest versions the user has been shown, for "new since last time".
    pub fn seen(&self, client: &Client) -> PathBuf {
        self.data_dir.join(client_key(client)).join("seen.toml")
    }

    pub fn lockfile(&self, client: &Client) -> PathBuf {
        self.data_dir
            .join(client_key(client))
            .join("kodo.lock.toml")
    }
}

/// Stable per-client directory name: flavor plus a hash of the AddOns path.
fn client_key(client: &Client) -> String {
    let path = client.addons.to_string_lossy();
    let path = if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.into_owned()
    };
    format!(
        "{}-{:08x}",
        client.flavor.id(),
        crate::fnv1a(path.as_bytes()) as u32
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    System,
}

/// Row height in the app's tables.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourcesConfig {
    pub github: bool,
    pub wowinterface: bool,
    pub curseforge: bool,
    /// Override where the GitHub addon catalogue is fetched from (upstream stays the fallback).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_url: Option<String>,
}

impl Default for SourcesConfig {
    fn default() -> Self {
        Self {
            github: true,
            wowinterface: true,
            curseforge: false,
            catalog_url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    pub name: String,
    pub flavor: Flavor,
    /// The client's `Interface/AddOns` folder.
    pub path: PathBuf,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowConfig {
    pub width: f32,
    pub height: f32,
    pub x: Option<f32>,
    pub y: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: Theme,
    pub density: Density,
    /// Zoom for the whole app window (1.0 = 100%).
    pub ui_scale: f32,
    pub concurrency: usize,
    pub check_on_launch: bool,
    /// Re-check for updates now and then, and tell the OS when new ones turn up while the
    /// window isn't focused.
    pub notify: bool,
    pub sources: SourcesConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowConfig>,
    /// Table column widths the user dragged, by `table.column`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub columns: BTreeMap<String, f32>,
    /// Each table's sort as `column:asc|desc`, by table.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub sorts: BTreeMap<String, String>,
    #[serde(rename = "client", skip_serializing_if = "Vec::is_empty")]
    pub clients: Vec<ClientConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::Dark,
            density: Density::Comfortable,
            ui_scale: 1.0,
            concurrency: 4,
            check_on_launch: true,
            notify: true,
            sources: SourcesConfig::default(),
            window: None,
            columns: BTreeMap::new(),
            sorts: BTreeMap::new(),
            clients: Vec::new(),
        }
    }
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl Config {
    /// Load the config; a missing file is the default config.
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
        write_atomic(path, text.as_bytes())
    }

    /// The clients to manage: the configured ones once any exist, otherwise whatever detection
    /// finds. Errors are per-location and non-fatal.
    pub fn clients(&self) -> (Vec<Client>, Vec<Error>) {
        if self.clients.is_empty() {
            let found = detect::detect();
            return (found.clients, found.errors);
        }
        let mut clients = Vec::new();
        let mut errors = Vec::new();
        for cfg in self.clients.iter().filter(|c| c.enabled) {
            match detect::from_path(&cfg.path) {
                Ok(found) => clients.extend(found.into_iter().take(1).map(|mut c| {
                    c.name.clone_from(&cfg.name);
                    c
                })),
                Err(e) => errors.push(e),
            }
        }
        (clients, errors)
    }

    /// Add or replace the config entry for a client.
    pub fn upsert_client(&mut self, client: &Client) {
        let entry = ClientConfig {
            name: client.name.clone(),
            flavor: client.flavor.clone(),
            path: client.addons.clone(),
            enabled: true,
        };
        match self.clients.iter_mut().find(|c| c.path == client.addons) {
            Some(existing) => *existing = entry,
            None => self.clients.push(entry),
        }
    }
}

/// Which secret to look up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secret {
    GithubToken,
    CurseforgeKey,
    WagoKey,
}

impl Secret {
    pub fn env(self) -> &'static str {
        match self {
            Self::GithubToken => "GITHUB_TOKEN",
            Self::CurseforgeKey => "KODO_CURSEFORGE_KEY",
            Self::WagoKey => "KODO_WAGO_KEY",
        }
    }

    fn account(self) -> &'static str {
        match self {
            Self::GithubToken => "github-token",
            Self::CurseforgeKey => "curseforge-key",
            Self::WagoKey => "wago-key",
        }
    }

    fn entry(self) -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, self.account())
            .map_err(|e| Error::Keyring(e.to_string()))
    }

    /// The env var if set and non-empty (it wins, for CI and Nix), else the OS keychain.
    /// Blocking: call it from a blocking context, not on an async executor thread.
    pub fn get(self) -> Option<String> {
        std::env::var(self.env())
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.entry().ok()?.get_password().ok())
            .filter(|v| !v.trim().is_empty())
    }

    /// Whether the value comes from the environment (so the keychain copy is ignored).
    pub fn from_env(self) -> bool {
        std::env::var(self.env()).is_ok_and(|v| !v.trim().is_empty())
    }

    /// Store in the OS keychain; an empty value deletes it.
    pub fn set(self, value: &str) -> Result<()> {
        let entry = self.entry()?;
        let res = if value.trim().is_empty() {
            match entry.delete_credential() {
                Err(keyring::Error::NoEntry) => Ok(()),
                other => other,
            }
        } else {
            entry.set_password(value.trim())
        };
        res.map_err(|e| Error::Keyring(e.to_string()))
    }
}

const KEYRING_SERVICE: &str = "dev.cobi.kodo";

/// Write a file by renaming a sibling temp file over it, creating parent dirs as needed.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(io_err("create", dir))?;
    let mut tmp =
        tempfile::NamedTempFile::new_in(dir).map_err(io_err("create a temp file in", dir))?;
    tmp.write_all(bytes)
        .map_err(io_err("write", tmp.path().to_path_buf()))?;
    tmp.persist(path)
        .map_err(|e| io_err("write", path)(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::config::{Config, Theme, move_dir};
    use crate::flavor::Flavor;

    #[test]
    fn moves_old_windows_dirs_once() {
        let tmp = tempfile::tempdir().unwrap();
        let roaming = tmp.path();
        let (old, new) = (roaming.join("cobi/kodo/data"), roaming.join("kodo/data"));
        std::fs::create_dir_all(old.join("forever-1")).unwrap();
        std::fs::write(old.join("forever-1/kodo.lock.toml"), "x").unwrap();
        std::fs::create_dir_all(roaming.join("cobi/kodo/config")).unwrap();

        assert_eq!(move_dir(&old, &new), new);
        assert!(new.join("forever-1/kodo.lock.toml").is_file());
        assert!(roaming.join("cobi/kodo").is_dir(), "config is still there");
        let (old_cfg, new_cfg) = (
            roaming.join("cobi/kodo/config"),
            roaming.join("kodo/config"),
        );
        assert_eq!(move_dir(&old_cfg, &new_cfg), new_cfg);
        assert!(!roaming.join("cobi").exists(), "emptied parents go");
        // Nothing left to move: the new dir as is.
        assert_eq!(move_dir(&old, &new), new);
    }

    #[test]
    fn parses_spec_example_and_round_trips() {
        let text = r#"
theme = "light"
concurrency = 2

[sources]
curseforge = true

[[client]]
name = "Forever"
flavor = "forever"
path = 'C:\Program Files (x86)\World of Warcraft\_forever_\Interface\AddOns'
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.theme, Theme::Light);
        assert_eq!(cfg.concurrency, 2);
        assert!(cfg.check_on_launch, "missing keys take defaults");
        assert!(cfg.sources.github && cfg.sources.curseforge);
        assert_eq!(cfg.clients[0].flavor, Flavor::Forever);
        assert!(cfg.clients[0].enabled);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
        assert_eq!(
            Config::load(&dir.path().join("missing.toml")).unwrap(),
            Config::default()
        );
    }
}
