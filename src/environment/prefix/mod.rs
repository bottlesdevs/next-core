//! Prefix backends own initialization, software materialization, and storage release.
//! Environment stops the owner's runtime before calling apply, prepare, or release.
//! Backends stop their own initialization and installer processes before returning.

mod standard;
#[cfg(feature = "fvs")]
mod virgo;
#[cfg(feature = "fvs")]
pub use virgo::VirgoError;
#[cfg(feature = "fvs")]
pub(crate) use virgo::VirgoManager;

use super::EnvironmentState;
use crate::{Addons, Context, Progress, error::Result, runner::Runner};
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
    /// Create initial prefix data. A successful return leaves no Wine processes running.
    /// Failed cleanup retains data for explicit recovery by the caller.
    pub(super) async fn create(
        &self,
        config: &EnvironmentState,
        root: &Path,
        cx: &Context,
    ) -> Result<()> {
        match self {
            Self::Standard => standard::create(config, root, cx).await,
            #[cfg(feature = "fvs")]
            Self::Virgo => Ok(async_fs::create_dir_all(root.join("upper")).await?),
        }
    }

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

    /// Apply validated selections to a stopped prefix. Virgo defers materialization.
    pub(super) async fn apply(
        &self,
        previous: &EnvironmentState,
        candidate: &EnvironmentState,
        root: &Path,
        cx: &Context,
        addons: &Addons,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        match self {
            Self::Standard => {
                standard::apply(
                    previous,
                    candidate,
                    root,
                    cx,
                    addons,
                    progress,
                    cancellation,
                )
                .await
            }
            #[cfg(feature = "fvs")]
            Self::Virgo => Ok(()),
        }
    }

    /// Materialize a stopped prefix for execution using the selected configuration.
    /// Backends undo partial owner materialization before returning an error.
    pub(super) async fn prepare(
        &self,
        config: &EnvironmentState,
        runner: &dyn Runner,
        root: &Path,
        cx: &Context,
        #[cfg(feature = "fvs")] virgo: &VirgoManager,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        #[cfg(not(feature = "fvs"))]
        let _ = (config, runner, root, cx, progress, cancellation);
        match self {
            Self::Standard => Ok(()),
            #[cfg(feature = "fvs")]
            Self::Virgo => {
                virgo::prepare(config, runner, root, cx, virgo, progress, cancellation).await
            }
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
