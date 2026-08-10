//! Snapshot history operations.

use std::path::Path;

use fvs_rs::{Repository, RestoreResponse};

use crate::{
    Operation, Progress, Stage, Transfer,
    error::{Error, Result},
    prefix::{AUTO_CHECKPOINT_MESSAGE, FVS_BLOCK_SIZE, finish_commit, finish_restore},
};

use super::{Bottle, Snapshot, SnapshotSummary, error::BottleError, state::BottleState};

impl Bottle {
    /// Saves the bottle's current files and configuration in snapshot history.
    ///
    /// The operation takes exclusive bottle access and stops the bottle before
    /// inspecting the complete library-managed bottle directory, including
    /// `bottle.toml`.
    ///
    /// If the tree has not changed, no history entry is created. The returned
    /// [`Snapshot`] then has `created == false`, and its state ID, message, and
    /// timestamp describe the pre-existing FVS head rather than `message`.
    /// The message `bottles-next:auto-checkpoint` is reserved for internal
    /// transactions; snapshots using it are hidden by [`snapshots`](Self::snapshots).
    ///
    /// Cancellation is observed after stopping and before the FVS commit
    /// begins. Once streaming starts, this operation does not check for
    /// cancellation again.
    ///
    /// # Errors
    ///
    /// The operation returns an error if the bottle was deleted, cannot be
    /// stopped, the FVS service is unavailable, cancellation is requested, or
    /// the snapshot cannot be created.
    pub fn create_snapshot(&self, message: impl Into<String>) -> Operation<Snapshot> {
        let bottle = self.clone();
        let repository = self.snapshot_repository();
        let cx = self.0.cx.clone();
        let message = message.into();
        Operation::new(move |progress, cancellation| async move {
            let _write = bottle.0.write_lock.write().await;
            let state = bottle.state()?;
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            Bottle::stop_state(&state, &cx).await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let stream = cx.fvs().await?.commit_stream(&repository, message).await?;
            finish_commit(stream, |update| {
                progress.send_replace(Some(Progress::transferring(
                    Stage::Committing,
                    Transfer::from(update),
                )));
            })
            .await
        })
    }

    /// Lists caller-visible snapshots for this bottle.
    ///
    /// Results are newest-first. Every commit whose message is exactly
    /// `bottles-next:auto-checkpoint` is excluded because that value is reserved
    /// for internal mutation checkpoints.
    ///
    /// Listing holds shared bottle access: WineBridge requests may continue,
    /// while edits, stop, snapshot mutation, and deletion wait.
    ///
    /// # Errors
    ///
    /// Returns an error if the bottle was deleted, the FVS service is
    /// unavailable, or its snapshot history cannot be read.
    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        let _read = self.0.write_lock.read().await;
        self.ensure_exists()?;
        let repository = self.snapshot_repository();
        Ok(self
            .0
            .cx
            .fvs()
            .await?
            .list_commits(&repository)
            .await?
            .into_iter()
            .filter(|snapshot| snapshot.message != AUTO_CHECKPOINT_MESSAGE)
            .collect())
    }

    /// Restores the bottle to a snapshot selected by full state ID or prefix.
    ///
    /// The operation takes exclusive bottle access. It stops the bottle, then
    /// replaces the complete bottle tree with the target;
    /// files absent from that snapshot are removed. The state being replaced is
    /// not saved automatically. On success, the returned string is the resolved
    /// full state ID and the restored `bottle.toml` is published as a new
    /// [`BottleState`] snapshot.
    ///
    /// Currently this restores the working files without moving FVS's current
    /// commit to the target. Cancellation is observed before restore begins,
    /// but not while the FVS stream is running.
    ///
    /// Restore changes the filesystem before loading and validating the
    /// restored metadata. If that final step fails, the operation returns an
    /// error after disk contents have changed, while the previously published
    /// live state remains in place.
    ///
    /// # Errors
    ///
    /// The operation returns an error if the bottle cannot be stopped, the
    /// target is missing or ambiguous, the restore fails, cancellation is
    /// requested, or the restored metadata has a different bottle UUID.
    pub fn rollback(&self, state_id_or_prefix: &str) -> Operation<String> {
        let bottle = self.clone();
        let repository = self.snapshot_repository();
        let bottle_path = self.bottle_path();
        let cx = self.0.cx.clone();
        let state_id_or_prefix = state_id_or_prefix.to_owned();
        Operation::new(move |progress, cancellation| async move {
            let _write = bottle.0.write_lock.write().await;
            let state = bottle.state()?;
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            Bottle::stop_state(&state, &cx).await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }

            bottle.ensure_exists()?;
            let stream = cx
                .fvs()
                .await?
                .restore_stream(&repository, &state_id_or_prefix, None::<&Path>, true, false)
                .await?;
            let response: RestoreResponse = finish_restore(stream, |update| {
                progress.send_replace(Some(Progress::transferring(
                    Stage::Restoring,
                    Transfer::from(update),
                )));
            })
            .await?;
            let path = bottle_path.join("bottle.toml");
            let state: BottleState = next_config::load(path).await?;
            if state.id != bottle.0.id {
                return Err(BottleError::IdMismatch {
                    expected: bottle.0.id,
                    actual: state.id,
                }
                .into());
            }
            bottle.publish(state);
            Ok(response.state_id)
        })
    }

    /// Addresses the history repository that every bottle owns independently
    /// of its prefix storage strategy.
    fn snapshot_repository(&self) -> Repository {
        Repository {
            repository_path: self.bottle_path().display().to_string(),
            block_size: FVS_BLOCK_SIZE,
        }
    }
}
