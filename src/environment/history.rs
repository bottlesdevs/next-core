//! Captures and restores complete environment history with FVS.
//!
//! A snapshot includes the persisted owner state, private files, registry
//! baseline, and configuration. History operations run while holding owner
//! coordination and after runtime storage has been released. Automatic
//! checkpoints protect mutations and are hidden from user-facing listings.

use super::{BackendSource, Environment, State};
use crate::virgo::FVS_BLOCK_SIZE;
use crate::{
    Context, EnvironmentError, Operation, Progress, Stage, Transfer,
    error::{Error, Result},
};
pub use fvs_rs::{Commit as Snapshot, CommitSummary as SnapshotSummary};
use fvs_rs::{Commit, Progress as FvsProgress, Repository, RestoreResponse};
use std::{path::Path, sync::Arc};
use tokio::sync::watch;

/// Identifies internal rollback checkpoints hidden from user snapshot listings.
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

/// Describes the FVS repository rooted at `root` using the shared block size.
pub(crate) fn repository(root: &Path) -> Repository {
    Repository {
        repository_path: root.display().to_string(),
        block_size: FVS_BLOCK_SIZE,
    }
}

/// Commits the complete owner root, creating its FVS repository if necessary.
///
/// # Errors
///
/// Returns an error if repository discovery, repository creation, or commit fails.
pub(crate) async fn capture(
    root: &Path,
    message: String,
    allow_empty: bool,
    stage: Stage,
    cx: &Context,
    progress: &watch::Sender<Option<Progress>>,
) -> Result<Commit> {
    let client = cx.fvs();
    if !crate::utils::fs::exists(&root.join(".fvs2")).await? {
        client.new_repository(root, FVS_BLOCK_SIZE).await?;
    }
    Ok(client
        .commit_with_progress(&repository(root), message, allow_empty, |event| {
            progress.send_replace(Some(Progress::transferring(stage.clone(), event.into())));
        })
        .await?)
}

/// Restores `revision` into the owner root and reports transfer progress.
///
/// # Errors
///
/// Returns an error if FVS cannot resolve or restore the revision.
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

/// Returns a mutation result, restoring `checkpoint` first when it is an error.
///
/// Nothing is published before recovery succeeds. If both the mutation and the
/// restore fail, the returned [`EnvironmentError::Rollback`] identifies the root
/// that requires repair.
///
/// # Errors
///
/// Returns the original mutation error after a successful recovery, or
/// [`EnvironmentError::Rollback`] if recovery also fails.
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

impl<T: BackendSource> Environment<T>
where
    State<T>: next_config::Config + Clone + PartialEq + Send + Sync,
{
    /// Creates a user-visible snapshot after stopping the environment.
    ///
    /// Cancellation is observed before capture starts, but does not interrupt an
    /// active FVS commit.
    ///
    /// # Errors
    ///
    /// The operation fails for the reserved checkpoint message, cancellation,
    /// shutdown failures, or FVS repository and commit errors.
    pub(crate) fn create_snapshot(self: &Arc<Self>, message: String) -> Operation<Snapshot> {
        let environment = self.clone();
        Operation::new(move |progress, cancellation| async move {
            if message == AUTO_CHECKPOINT_MESSAGE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "snapshot message is reserved for internal checkpoints",
                )
                .into());
            }
            let _control = environment.lock_control(&cancellation).await?;
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            environment.stop_locked().await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            capture(
                &environment.root,
                message,
                true,
                Stage::Committing,
                &environment.context,
                &progress,
            )
            .await
        })
    }

    /// Lists user-visible snapshots in the environment repository.
    ///
    /// Returns an empty list before the first snapshot and excludes automatic
    /// recovery checkpoints.
    ///
    /// # Errors
    ///
    /// Returns an error if the environment was deleted or FVS cannot list commits.
    pub(crate) async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        let _control = self.control.lock().await;
        self.state()?;
        if !crate::utils::fs::exists(&self.root.join(".fvs2")).await? {
            return Ok(Vec::new());
        }
        Ok(self
            .context
            .fvs()
            .list_commits(&repository(&self.root))
            .await?
            .into_iter()
            .filter(|snapshot| snapshot.message != AUTO_CHECKPOINT_MESSAGE)
            .collect())
    }

    /// Restores a snapshot and publishes the validated state stored within it.
    ///
    /// An automatic checkpoint is captured before restoration. Once restoration
    /// begins, a failed restore, load, identity check, backend check, or validation
    /// triggers recovery to that checkpoint before coordination is released.
    /// Cancellation is sampled before and after checkpoint capture; once restore
    /// starts, the operation finishes restoration or recovery before returning.
    ///
    /// # Errors
    ///
    /// The operation fails for cancellation before restoration, shutdown or FVS
    /// failures, invalid restored state, or [`EnvironmentError::Rollback`] if the
    /// automatic recovery also fails.
    pub(crate) fn rollback(self: &Arc<Self>, revision: &str) -> Operation<String> {
        let environment = self.clone();
        let revision = revision.to_owned();
        Operation::new(move |progress, cancellation| async move {
            let _control = environment.lock_control(&cancellation).await?;
            let current = environment.state()?;
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            environment.stop_locked().await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let checkpoint = capture(
                &environment.root,
                AUTO_CHECKPOINT_MESSAGE.into(),
                false,
                Stage::Checkpointing,
                &environment.context,
                &progress,
            )
            .await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            // Once restoration starts, finish it or recover before releasing coordination.
            let result = async {
                let restored = restore(
                    &environment.root,
                    &revision,
                    &environment.context,
                    &progress,
                )
                .await?;
                let state: State<T> =
                    next_config::load(environment.root.join("state.toml")).await?;
                if state.id() != current.id() {
                    return Err(EnvironmentError::IdMismatch {
                        expected: current.id(),
                        actual: state.id(),
                    }
                    .into());
                }
                if state.data.backend() != current.data.backend() {
                    return Err(EnvironmentError::InvalidEdit(
                        "snapshot backend does not match owner",
                    )
                    .into());
                }
                state.config.validate()?;
                Ok((restored.state_id, state))
            }
            .await;
            let (revision, state) = recover(
                result,
                &environment.root,
                &checkpoint,
                &environment.context,
                &progress,
            )
            .await?;
            environment.publish(state);
            Ok(revision)
        })
    }
}
