//! Prefix preparation and storage backends.
//!
//! Standard storage mutates a conventional prefix directly; Virgo stores an
//! ordered FVS layer stack with a private writable upper directory. Virgo
//! materialization uses rollback checkpoints; Standard uses FVS only for explicit snapshots.

mod standard;
#[cfg(feature = "fvs")]
mod virgo;

use std::path::Path;

#[cfg(feature = "fvs")]
pub use virgo::VirgoError;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{Addons, Context, EnvironmentConfig, Progress, error::Result};

#[cfg(feature = "fvs")]
pub(crate) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;

/// Selects conventional mutable storage or FVS composition.
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub enum Storage {
    /// Stores a conventional mutable prefix in the owner directory.
    ///
    /// Explicit snapshots may use FVS; ordinary mutations use direct writes.
    Standard,
    /// Stores the prefix as composable FVS layers.
    ///
    /// Virgo is experimental and requires the configured FVS service.
    #[cfg(feature = "fvs")]
    Virgo,
}

/// Creates storage at an explicit owner location.
pub(crate) async fn create(storage: &Storage, root: &Path) -> Result<()> {
    let directory = match storage {
        Storage::Standard => "prefix",
        #[cfg(feature = "fvs")]
        Storage::Virgo => "upper",
    };
    Ok(async_fs::create_dir_all(root.join(directory)).await?)
}

/// Checks backend restrictions before the owner stops or publishes an edit.
pub(crate) fn validate_edit(
    previous: &EnvironmentConfig,
    candidate: &EnvironmentConfig,
) -> Result<()> {
    if matches!(candidate.storage, Storage::Standard)
        && !candidate.dependencies.starts_with(&previous.dependencies)
    {
        return Err(crate::EnvironmentError::InvalidEdit(
            "installed dependencies cannot be removed, replaced or reordered",
        )
        .into());
    }
    Ok(())
}

/// Applies edited selections to a stopped prefix; Virgo defers work until preparation.
pub(crate) async fn reconcile(
    previous: &EnvironmentConfig,
    candidate: &EnvironmentConfig,
    root: &Path,
    cx: &Context,
    addons: &Addons,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    match candidate.storage {
        Storage::Standard => {
            standard::reconcile(
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
        Storage::Virgo => Ok(()),
    }
}

/// Prepares a stopped owner's prefix for execution.
pub(crate) async fn prepare(
    config: &EnvironmentConfig,
    root: &Path,
    cx: &Context,
    addons: &Addons,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    let _ = (root, cx, addons, progress, cancellation);
    match config.storage {
        Storage::Standard => Ok(()),
        #[cfg(feature = "fvs")]
        Storage::Virgo => virgo::prepare(config, root, cx, addons, progress, cancellation).await,
    }
}

pub(crate) async fn stop(storage: &Storage, root: &Path, context: &Context) -> Result<()> {
    let _ = (root, context);
    match storage {
        Storage::Standard => Ok(()),
        #[cfg(feature = "fvs")]
        Storage::Virgo => virgo::stop(root, context).await,
    }
}
