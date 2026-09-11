//! Owner history includes selected configuration, the registry baseline, and persistent data.
//! Callers hold owner coordination and stop Wine and mounts before using it.

use super::prefix::FVS_BLOCK_SIZE;
use crate::{Context, Progress, Stage, Transfer, error::Result};
use futures_core::Stream;
use futures_util::TryStreamExt;
use fvs_rs::{
    Commit, Progress as FvsProgress, Repository, RestoreResponse, error::Error as FvsError,
};
use std::path::Path;
use tokio::sync::watch;

/// Identifies rollback checkpoints that must not appear as user snapshots.
///
/// Snapshot filtering compares this persisted value exactly, so changing it
/// would expose checkpoints created by older versions.
pub(crate) const AUTO_CHECKPOINT_MESSAGE: &str = "bottles-next:auto-checkpoint";

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

pub(crate) fn repository(root: &Path) -> Repository {
    Repository {
        repository_path: root.display().to_string(),
        block_size: FVS_BLOCK_SIZE,
    }
}

pub(crate) async fn capture(
    root: &Path,
    message: String,
    allow_empty: bool,
    stage: Stage,
    cx: &Context,
    progress: &watch::Sender<Option<Progress>>,
) -> Result<Commit> {
    for directory in ["prefix", "upper"] {
        crate::winebridge::WineBridgeClient::clear_discovery(&root.join(directory)).await?;
    }
    let client = cx.fvs().await?;
    if !crate::utils::exists(&root.join(".fvs2")).await? {
        client.new_repository(root, FVS_BLOCK_SIZE).await?;
    }
    let stream = client
        .commit_stream(&repository(root), message, allow_empty)
        .await?;
    finish_commit(stream, |event| {
        progress.send_replace(Some(Progress::transferring(stage.clone(), event.into())));
    })
    .await
}

pub(crate) async fn restore(
    root: &Path,
    revision: &str,
    cx: &Context,
    progress: &watch::Sender<Option<Progress>>,
) -> Result<RestoreResponse> {
    let stream = cx
        .fvs()
        .await?
        .restore_stream(&repository(root), revision, None::<&Path>, true, false)
        .await?;
    finish_restore(stream, |event| {
        progress.send_replace(Some(Progress::transferring(Stage::Restoring, event.into())));
    })
    .await
}

/// Restore both data and configuration on a rejected mutation. Nothing is published
/// before this succeeds; a failed rollback names the owner requiring repair.
pub(crate) async fn recover<T>(
    result: Result<T>,
    root: &Path,
    checkpoint: &Commit,
    cx: &Context,
    progress: &watch::Sender<Option<Progress>>,
) -> Result<T> {
    if let Err(error) = &result {
        if let Err(source) = restore(root, &checkpoint.state_id, cx, progress).await {
            tracing::error!(%error, "mutation failed before rollback failed");
            return Err(super::EnvironmentError::Rollback {
                root: root.to_path_buf(),
                source: Box::new(source),
            }
            .into());
        }
    }
    result
}

/// Drains an FVS commit stream, forwarding every frame and requiring a terminal commit.
async fn finish_commit(
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
async fn finish_restore(
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

#[cfg(test)]
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
