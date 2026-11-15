//! The contract every addon source adapter (in `kodo-sources`) implements.
//!
//! Core owns the trait so the engine can drive sources without depending on any HTTP stack;
//! the binaries build concrete sources and hand them to [`crate::engine::Engine`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::addon::{AddonRef, Change, Channel, Match, Query, Release, RemoteAddon, SourceId};
use crate::flavor::Flavor;
use crate::local::LocalAddon;

/// Download progress callback: `(bytes so far, total if known)`.
pub type Progress = Arc<dyn Fn(u64, Option<u64>) + Send + Sync>;

#[async_trait]
pub trait Source: Send + Sync {
    fn id(&self) -> SourceId;

    async fn search(&self, q: &Query, flavor: &Flavor) -> Result<Vec<RemoteAddon>, SourceError>;

    /// Newest release for the flavor that the channel accepts.
    async fn resolve(
        &self,
        addon: &AddonRef,
        flavor: &Flavor,
        channel: Channel,
    ) -> Result<Release, SourceError>;

    /// A specific version, for `pin --version`.
    async fn resolve_version(
        &self,
        _addon: &AddonRef,
        _flavor: &Flavor,
        _version: &str,
    ) -> Result<Release, SourceError> {
        Err(SourceError::Unsupported {
            source_id: self.id(),
            what: "installing a specific version",
        })
    }

    /// Identify installed folders this source can vouch for (e.g. CurseForge fingerprints).
    async fn match_installed(&self, _local: &[LocalAddon]) -> Result<Vec<Match>, SourceError> {
        Ok(Vec::new())
    }

    /// Name-based guesses for folders nothing else matched. Shown to the user to confirm;
    /// never adopted automatically.
    async fn suggest(
        &self,
        _local: &[LocalAddon],
        _flavor: &Flavor,
    ) -> Result<Vec<Match>, SourceError> {
        Ok(Vec::new())
    }

    /// The addon's long description (a GitHub README), as Markdown; `None` if it has none.
    async fn readme(&self, _addon: &AddonRef) -> Result<Option<String>, SourceError> {
        Ok(None)
    }

    /// Notes for every release on `channel` newer than version `since`, newest first. `None`
    /// when the source only describes its latest release.
    async fn changes(
        &self,
        _addon: &AddonRef,
        _channel: Channel,
        _since: &str,
    ) -> Result<Option<Vec<Change>>, SourceError> {
        Ok(None)
    }

    /// Stream `url` to `dest`, returning the number of bytes written.
    async fn download(
        &self,
        url: &str,
        dest: &Path,
        progress: Option<Progress>,
    ) -> Result<u64, SourceError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("{0} was not found")]
    NotFound(String),
    #[error("{addon} {version} has no build for {flavor}")]
    NoAsset {
        addon: AddonRef,
        version: String,
        flavor: String,
    },
    #[error("{0} has no release on the requested channel")]
    NoRelease(AddonRef),
    #[error("{} needs an API key; add one in Settings or set {env}", .source_id.label())]
    MissingKey {
        source_id: SourceId,
        env: &'static str,
    },
    #[error("{} rate limit hit; {hint}", .source_id.label())]
    RateLimited { source_id: SourceId, hint: String },
    #[error("{url}: HTTP {status}")]
    Http { url: String, status: u16 },
    #[error("{url}: {message}")]
    Network { url: String, message: String },
    #[error("{url}: unexpected response: {message}")]
    Parse { url: String, message: String },
    #[error("{} does not support {what}", .source_id.label())]
    Unsupported {
        source_id: SourceId,
        what: &'static str,
    },
    #[error("failed to write {}: {err}", path.display())]
    Io { path: PathBuf, err: std::io::Error },
}
