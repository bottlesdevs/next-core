//! Owner history includes selected configuration, the registry baseline, and persistent data.
//! Callers hold owner coordination and release runtime storage before using it.

use super::prefix::FVS_BLOCK_SIZE;
use crate::{Context, Progress, Stage, Transfer, error::Result};
use fvs_rs::{Commit, Progress as FvsProgress, Repository, RestoreResponse};
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
    let client = cx.fvs();
    if !crate::utils::exists(&root.join(".fvs2")).await? {
        client.new_repository(root, FVS_BLOCK_SIZE).await?;
    }
    Ok(client
        .commit_with_progress(&repository(root), message, allow_empty, |event| {
            progress.send_replace(Some(Progress::transferring(stage.clone(), event.into())));
        })
        .await?)
}

pub(crate) async fn restore(
    root: &Path,
    revision: &str,
    cx: &Context,
    progress: &watch::Sender<Option<Progress>>,
) -> Result<RestoreResponse> {
    Ok(cx
        .fvs()
        .restore_with_progress(
            &repository(root),
            revision,
            None::<&Path>,
            true,
            false,
            |event| {
                progress.send_replace(Some(Progress::transferring(Stage::Restoring, event.into())));
            },
        )
        .await?)
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
