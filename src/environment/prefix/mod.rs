//! Prefix storage backends and FVS history primitives.
//!
//! Standard storage mutates a conventional prefix directly; Virgo stores an
//! ordered FVS layer stack with a private writable upper directory. Virgo addon
//! changes use rollback checkpoints; Standard uses FVS only for explicit snapshots.

mod standard;
#[cfg(feature = "fvs")]
mod virgo;

use std::path::Path;

#[cfg(feature = "fvs")]
pub use virgo::VirgoError;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[cfg(feature = "fvs")]
use {
    crate::Transfer,
    futures_core::Stream,
    futures_util::TryStreamExt,
    fvs_rs::{Commit, Layer, Progress as FvsProgress, RestoreResponse, error::Error as FvsError},
};

use crate::{Context, error::Result, runner::Runner};

/// Identifies rollback checkpoints that must not appear as user snapshots.
///
/// Snapshot filtering compares this persisted value exactly, so changing it
/// would expose checkpoints created by older versions.
#[cfg(feature = "fvs")]
pub(crate) const AUTO_CHECKPOINT_MESSAGE: &str = "bottles-next:auto-checkpoint";
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
    Virgo {
        /// Resolved layer order retained until composition is derived from settings.
        #[serde(default)]
        layers: Vec<Layer>,
    },
}

#[cfg(feature = "fvs")]
impl From<&FvsProgress> for Transfer {
    fn from(progress: &FvsProgress) -> Self {
        // FVS uses negative counters when progress is unavailable. Core progress
        // uses unsigned values and represents an unavailable total explicitly.
        Self {
            current: progress.current.try_into().unwrap_or_default(),
            total: progress.total.try_into().ok().filter(|total| *total > 0),
        }
    }
}

/// Creates storage at an explicit owner location.
pub(crate) async fn create(
    storage: &mut Storage,
    root: &Path,
    runner: &dyn Runner,
    runner_key: &str,
    context: &Context,
    addons: &crate::Addons,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    #[cfg(not(feature = "fvs"))]
    let _ = (runner_key, context, addons, cancellation);
    match storage {
        Storage::Standard => standard::create(&root.join("prefix"), runner).await,
        #[cfg(feature = "fvs")]
        Storage::Virgo { layers } => {
            *layers =
                virgo::create(root, runner, runner_key, context, addons, cancellation).await?;
            Ok(())
        }
    }
}

pub(crate) async fn prepare(storage: &Storage, root: &Path, context: &Context) -> Result<()> {
    let _ = (root, context);
    match storage {
        Storage::Standard => Ok(()),
        #[cfg(feature = "fvs")]
        Storage::Virgo { layers } => virgo::prepare(root, layers, context).await,
    }
}

pub(crate) async fn stop(storage: &Storage, root: &Path, context: &Context) -> Result<()> {
    let _ = (root, context);
    match storage {
        Storage::Standard => Ok(()),
        #[cfg(feature = "fvs")]
        Storage::Virgo { .. } => virgo::stop(root, context).await,
    }
}

pub(crate) async fn rebuild(
    storage: &mut Storage,
    runner: &dyn Runner,
    runner_key: &str,
    installed: &[Uuid],
    context: &Context,
    addons: &crate::Addons,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    match storage {
        Storage::Standard => {
            let _ = (runner, runner_key, installed, context, addons, cancellation);
            Ok(())
        }
        #[cfg(feature = "fvs")]
        Storage::Virgo { layers } => {
            virgo::rebuild(
                layers,
                runner,
                runner_key,
                installed,
                context,
                addons,
                cancellation,
            )
            .await
        }
    }
}

/// Applies one addon to storage; the enclosing software workflow owns cleanup.
pub(crate) async fn install(
    storage: &mut Storage,
    root: &Path,
    item_id: Uuid,
    replaced_id: Option<Uuid>,
    execute: impl for<'a> std::ops::AsyncFnOnce(&'a Path) -> Result<()>,
    context: &Context,
) -> Result<()> {
    let _ = (item_id, replaced_id, context);
    match storage {
        Storage::Standard => execute(&root.join("prefix")).await,
        #[cfg(feature = "fvs")]
        Storage::Virgo { layers } => {
            virgo::install(root, layers, item_id, replaced_id, context).await
        }
    }
}

pub(crate) async fn uninstall(
    storage: &mut Storage,
    root: &Path,
    item_id: Uuid,
    execute: impl for<'a> std::ops::AsyncFnOnce(&'a Path, bool) -> Result<()>,
    context: &Context,
) -> Result<()> {
    let _ = (item_id, context);
    match storage {
        Storage::Standard => execute(&root.join("prefix"), true).await,
        #[cfg(feature = "fvs")]
        Storage::Virgo { layers } => {
            virgo::uninstall(root, layers, item_id, execute, context).await
        }
    }
}

/// Drains an FVS commit stream, forwarding every frame and requiring a terminal commit.
#[cfg(feature = "fvs")]
pub(crate) async fn finish_commit(
    stream: impl Stream<Item = std::result::Result<FvsProgress, FvsError>>,
    on_progress: impl FnMut(&FvsProgress),
) -> Result<Commit> {
    finish_stream(
        stream,
        on_progress,
        |progress| progress.result_commit,
        "commit",
    )
    .await
}

/// Drains an FVS restore stream, forwarding every frame and requiring a terminal result.
#[cfg(feature = "fvs")]
pub(crate) async fn finish_restore(
    stream: impl Stream<Item = std::result::Result<FvsProgress, FvsError>>,
    on_progress: impl FnMut(&FvsProgress),
) -> Result<RestoreResponse> {
    finish_stream(
        stream,
        on_progress,
        |progress| progress.result_restore,
        "restore",
    )
    .await
}

/// Consumes the FVS streaming protocol and extracts its terminal payload.
///
/// Every frame, including the terminal frame, is forwarded to `on_progress`.
/// End-of-stream or a terminal frame without the expected payload is a protocol
/// error rather than successful completion.
#[cfg(feature = "fvs")]
async fn finish_stream<T>(
    stream: impl Stream<Item = std::result::Result<FvsProgress, FvsError>>,
    mut on_progress: impl FnMut(&FvsProgress),
    mut result: impl FnMut(FvsProgress) -> Option<T>,
    operation: &'static str,
) -> Result<T> {
    futures_util::pin_mut!(stream);
    while let Some(progress) = stream.try_next().await? {
        on_progress(&progress);
        if progress.done {
            return result(progress).ok_or(FvsError::MissingStreamResult(operation).into());
        }
    }
    Err(FvsError::MissingStreamResult(operation).into())
}

#[cfg(all(test, feature = "fvs"))]
mod fvs_tests {
    use futures_util::stream;

    use super::*;

    #[test]
    fn finish_commit_forwards_progress_and_returns_terminal_result() {
        futures_lite::future::block_on(async {
            let frames = [
                FvsProgress {
                    phase: "hashing".into(),
                    current: 1,
                    total: 2,
                    ..Default::default()
                },
                FvsProgress {
                    phase: "indexing".into(),
                    current: -1,
                    total: -1,
                    ..Default::default()
                },
                FvsProgress {
                    phase: "done".into(),
                    done: true,
                    result_commit: Some(Commit {
                        state_id: "checkpoint".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ];
            let mut updates = Vec::new();

            let commit = finish_commit(stream::iter(frames.map(Ok::<_, FvsError>)), |progress| {
                updates.push(Transfer::from(progress))
            })
            .await
            .unwrap();

            assert_eq!(
                updates,
                [
                    Transfer {
                        current: 1,
                        total: Some(2),
                    },
                    Transfer {
                        current: 0,
                        total: None,
                    },
                    Transfer {
                        current: 0,
                        total: None,
                    },
                ]
            );
            assert_eq!(commit.state_id, "checkpoint");
        });
    }
}
