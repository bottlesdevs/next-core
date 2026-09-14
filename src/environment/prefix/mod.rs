//! Prefix backends own initialization, software materialization, and storage release.
//! Environment stops the owner's runtime before calling apply, prepare, or release.
//! Backends stop their own initialization and installer processes before returning.

pub(super) mod standard;
#[cfg(feature = "fvs")]
mod virgo;
#[cfg(feature = "fvs")]
pub use virgo::VirgoError;
#[cfg(feature = "fvs")]
pub(crate) use virgo::VirgoManager;

use super::EnvironmentState;
use crate::{Context, Progress, error::Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "fvs")]
pub(super) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;

/// Selects how a runnable Wine prefix is created and maintained.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Serialize)]
pub enum PrefixBackend {
    /// Initialize and mutate a conventional prefix directly.
    /// Explicit snapshots may use FVS; ordinary mutations use direct writes.
    Standard,
    /// Build immutable artifacts and compose them with a private writable upper.
    /// Virgo is experimental and requires the configured FVS service.
    #[cfg(feature = "fvs")]
    Virgo,
}

impl PrefixBackend {
    /// Check backend-specific edit restrictions before the owner is stopped or changed.
    pub(super) fn validate_edit(
        &self,
        previous: &EnvironmentState,
        candidate: &EnvironmentState,
    ) -> Result<()> {
        match self {
            Self::Standard => standard::validate_edit(previous, candidate),
            #[cfg(feature = "fvs")]
            Self::Virgo => Ok(()),
        }
    }

    /// Assemble a stopped prefix for execution from already-installed effects.
    /// Backends undo partial owner materialization before returning an error.
    pub(super) async fn prepare(
        &self,
        config: &EnvironmentState,
        root: &Path,
        cx: &Context,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        #[cfg(not(feature = "fvs"))]
        let _ = (config, root, cx, progress, cancellation);
        match self {
            Self::Standard => Ok(()),
            #[cfg(feature = "fvs")]
            Self::Virgo => virgo::prepare(config, root, cx, progress, cancellation).await,
        }
    }

    /// Release storage and discovery files after process shutdown, before history capture.
    pub(super) async fn release(&self, root: &Path, cx: &Context) -> Result<()> {
        #[cfg(not(feature = "fvs"))]
        let _ = cx;
        match self {
            Self::Standard => {
                crate::winebridge::WineBridgeClient::clear_discovery(&root.join("prefix")).await
            }
            #[cfg(feature = "fvs")]
            Self::Virgo => virgo::release(root, cx).await,
        }
    }
}
