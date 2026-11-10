//! Captures and restores complete prefix history with FVS.
//!
//! A snapshot includes the persisted prefix state, private files, registry
//! baseline, and configuration. History operations run while holding prefix
//! coordination and after runtime storage has been released. Automatic
//! checkpoints protect mutations and are hidden from user-facing listings.

use super::{Prefix, PrefixState};
use crate::virgo::FVS_BLOCK_SIZE;
use crate::{Operation, PrefixError, Stage, Transfer, error::Result, operation::ProgressReporter};
pub use fvs_rs::{Commit as Snapshot, CommitSummary as SnapshotSummary};
use fvs_rs::{Commit, Fvs2dClient, Progress as FvsProgress, Repository, RestoreResponse};
use std::path::Path;

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

/// Commits the complete prefix root, creating its FVS repository if necessary.
///
/// # Errors
///
/// Returns an error if repository discovery, repository creation, or commit fails.
pub(crate) async fn capture(
    root: &Path,
    message: String,
    allow_empty: bool,
    client: &Fvs2dClient,
    progress: &ProgressReporter,
) -> Result<Commit> {
    if !crate::utils::fs::exists(&root.join(".fvs2")).await? {
        client.new_repository(root, FVS_BLOCK_SIZE).await?;
    }
    Ok(client
        .commit_with_progress(&repository(root), message, allow_empty, |event| {
            progress.transfer(event.into());
        })
        .await?)
}

/// Restores `revision` into the prefix root and reports transfer progress.
///
/// # Errors
///
/// Returns an error if FVS cannot resolve or restore the revision.
pub(crate) async fn restore(
    root: &Path,
    revision: &str,
    client: &Fvs2dClient,
    progress: &ProgressReporter,
) -> Result<RestoreResponse> {
    Ok(client
        .restore_with_progress(
            &repository(root),
            revision,
            None::<&Path>,
            true,
            false,
            |event| {
                progress.transfer(event.into());
            },
        )
        .await?)
}

/// Returns a mutation result, restoring `checkpoint` first when it is an error.
///
/// Nothing is published before recovery succeeds. If both the mutation and the
/// restore fail, the returned [`PrefixError::Rollback`] identifies the root
/// that requires repair.
///
/// # Errors
///
/// Returns the original mutation error after a successful recovery, or
/// [`PrefixError::Rollback`] if recovery also fails.
pub(crate) async fn recover<T>(
    result: Result<T>,
    root: &Path,
    checkpoint: &Commit,
    client: &Fvs2dClient,
    progress: &ProgressReporter,
) -> Result<T> {
    if let Err(error) = &result {
        progress.stage(Stage::Restoring);
        if let Err(source) = restore(root, &checkpoint.state_id, client, progress).await {
            tracing::error!(%error, "mutation failed before rollback failed");
            return Err(PrefixError::Rollback {
                root: root.to_path_buf(),
                source: Box::new(source),
            }
            .into());
        }
    }
    result
}

impl Prefix {
    /// Creates an operation that captures the prefix's managed state.
    ///
    /// Shared artifacts and external files are not copied or rebuilt on
    /// restoration. Explicit snapshots create a revision even without changes.
    /// The prefix is stopped before capture and is not restarted afterward.
    /// Shutdown and capture are not interrupted by cancellation;
    /// [`Operation::cancel`] waits for the current step to finish.
    ///
    /// # Errors
    ///
    /// Awaiting the operation returns an invalid-input I/O error when `message`
    /// is the reserved internal checkpoint message. It can also fail during
    /// cancellation, Wine shutdown, repository initialization, or commit.
    pub fn create_snapshot(&self, message: impl Into<String>) -> Operation<Snapshot> {
        let message = message.into();
        let prefix = self.clone();
        Operation::new(move |job| async move {
            if message == AUTO_CHECKPOINT_MESSAGE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "snapshot message is reserved for internal checkpoints",
                )
                .into());
            }
            let _control = job.cancellable(prefix.0.control.lock()).await?;
            job.transition(Stage::Stopping)?;
            prefix.stop_locked().await?;
            job.transition(Stage::Committing)?;
            capture(
                &prefix.0.root,
                message,
                true,
                &prefix.0.fvs.client,
                job.progress(),
            )
            .await
        })
    }

    /// Lists user snapshots from newest to oldest.
    ///
    /// Internal checkpoints are excluded. A prefix with no history
    /// returns an empty vector without contacting FVS.
    ///
    /// # Errors
    ///
    /// Returns an error if the prefix was deleted, history metadata cannot
    /// be read, or FVS cannot list revisions.
    pub async fn snapshots(&self) -> Result<Vec<crate::SnapshotSummary>> {
        let _control = self.0.control.lock().await;
        self.state()?;
        if !crate::utils::fs::exists(&self.0.root.join(".fvs2")).await? {
            return Ok(Vec::new());
        }
        Ok(self
            .0
            .fvs
            .client
            .list_commits(&repository(&self.0.root))
            .await?
            .into_iter()
            .filter(|snapshot| snapshot.message != AUTO_CHECKPOINT_MESSAGE)
            .collect())
    }

    /// Creates an operation that restores a snapshot into the managed root.
    ///
    /// A failed restore or unreadable restored state recovers the previous files
    /// before returning; failed recovery reports the root requiring repair.
    /// Shutdown and checkpoint capture finish before cancellation is observed.
    /// Once restoration begins, cancellation does not interrupt restoration or
    /// recovery. Working files change without moving the FVS repository's
    /// current commit. Rollback first attempts to stop the prefix and never
    /// restarts it.
    ///
    /// On success, the operation returns the full state ID resolved from
    /// `revision` and publishes the state stored in that revision.
    ///
    /// # Errors
    ///
    /// Awaiting the operation fails on cancellation before restoration, Wine
    /// shutdown or checkpoint failure, an unknown revision, mismatched or
    /// invalid restored state, or failed restoration or recovery.
    pub fn rollback(&self, revision: &str) -> Operation<String> {
        let prefix = self.clone();
        let revision = revision.to_owned();
        Operation::new(move |job| async move {
            let _control = job.cancellable(prefix.0.control.lock()).await?;
            prefix.state()?;
            job.transition(Stage::Stopping)?;
            prefix.stop_locked().await?;
            job.transition(Stage::Checkpointing)?;
            let checkpoint = capture(
                &prefix.0.root,
                AUTO_CHECKPOINT_MESSAGE.into(),
                false,
                &prefix.0.fvs.client,
                job.progress(),
            )
            .await?;
            job.transition(Stage::Restoring)?;
            // Once restoration starts, finish it or recover before releasing coordination.
            let result = async {
                let restored = restore(
                    &prefix.0.root,
                    &revision,
                    &prefix.0.fvs.client,
                    job.progress(),
                )
                .await?;
                let state: PrefixState =
                    next_config::load(prefix.0.root.join("state.toml")).await?;
                Ok((restored.state_id, state))
            }
            .await;
            let (revision, state) = recover(
                result,
                &prefix.0.root,
                &checkpoint,
                &prefix.0.fvs.client,
                job.progress(),
            )
            .await?;
            prefix.publish(state);
            Ok(revision)
        })
    }
}
