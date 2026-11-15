//! UI-free core shared by the kodo GUI and CLI.

pub mod addon;
pub mod build_info;
pub mod characters;
pub mod config;
pub mod detect;
pub mod engine;
pub mod flavor;
pub mod health;
pub mod history;
pub mod import;
pub mod install;
pub mod local;
pub mod lockfile;
pub mod profile;
pub mod source;
pub mod toc;
pub mod wtf;

use std::path::PathBuf;

pub use crate::source::SourceError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to {action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} is not a WoW install, client folder or Interface/AddOns folder", .0.display())]
    NotAClient(PathBuf),
    #[error("{0}")]
    BadRef(String),
    #[error("invalid {}: {message}", path.display())]
    BadFile { path: PathBuf, message: String },
    #[error("bad archive {}: {message}", path.display())]
    BadArchive { path: PathBuf, message: String },
    #[error("folder {folder} is already owned by {owner}; remove that addon first")]
    Conflict { folder: String, owner: String },
    #[error(
        "folder {0} exists but kodo doesn't manage it; pass --force to replace it (the old copy is kept in backups)"
    )]
    Unmanaged(String),
    #[error("{0} is not installed")]
    NotInstalled(String),
    #[error("{0} matches more than one installed addon: {1}")]
    Ambiguous(String, String),
    #[error("no backup of {0} to roll back to")]
    NoBackup(String),
    #[error("no profile named {0}")]
    NoProfile(String),
    #[error("{0} is running; close the game first so it doesn't load half-changed addons")]
    GameRunning(String),
    #[error("{name} is needed by {by}; remove anyway with --force")]
    Needed { name: String, by: String },
    #[error("{0} is disabled; enable it first")]
    Disabled(String),
    #[error("{0} is pinned; unpin it first")]
    Pinned(String),
    #[error("{name} can't be downloaded by third-party apps; get it from {page}")]
    Unavailable { name: String, page: String },
    #[error("{0} source is not enabled")]
    SourceDisabled(addon::SourceId),
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("OS keychain: {0}")]
    Keyring(String),
    #[error("background task failed: {0}")]
    Task(String),
}

impl Error {
    /// Network and source failures, as opposed to user or local errors (CLI exit code 3).
    pub fn is_network(&self) -> bool {
        matches!(self, Self::Source(_))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Attach the failing action and path to an io error.
pub(crate) fn io_err(
    action: &'static str,
    path: impl Into<PathBuf>,
) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io {
        action,
        path,
        source,
    }
}

/// Stable 64-bit FNV-1a, for cache and lockfile file names that must not change across builds.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Path to the shared test fixtures at the workspace root.
#[cfg(test)]
pub(crate) fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}
