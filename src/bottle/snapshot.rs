//! Snapshot history operations.

use crate::{
    Operation, Progress, Stage,
    environment::history::{self, AUTO_CHECKPOINT_MESSAGE},
    error::{Error, Result},
};

use super::{Bottle, Snapshot, SnapshotSummary, error::BottleError, state::BottleState};

impl Bottle {
    /// Saves the bottle's current files and configuration in snapshot history.
    ///
    /// The operation takes exclusive bottle access and stops the bottle before
    /// inspecting the complete library-managed bottle directory, including
    /// `bottle.toml` and the existing registry baseline. Pending selections remain
    /// pending; this operation does not build artifacts or prepare a new composition.
    /// Standard history is initialized on the first snapshot.
    ///
    /// Explicit snapshots always create a new commit with the requested message,
    /// even when the files have not changed. The message
    /// `bottles-next:auto-checkpoint` is reserved and rejected.
    ///
    /// Cancellation is observed after stopping and before the snapshot commit.
    /// Once that stream starts, it is drained without cancellation.
    ///
    /// # Errors
    ///
    /// The operation returns an error if the bottle was deleted, cannot be
    /// stopped, the FVS service is unavailable, cancellation is requested, or
    /// the snapshot cannot be created.
    pub fn create_snapshot(&self, message: impl Into<String>) -> Operation<Snapshot> {
        let bottle = self.clone();
        let cx = self.0.cx.clone();
        let message = message.into();
        Operation::new(move |progress, cancellation| async move {
            if message == AUTO_CHECKPOINT_MESSAGE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "snapshot message is reserved for internal checkpoints",
                )
                .into());
            }
            let _control = cancellation
                .run_until_cancelled(bottle.0.control.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            bottle.stop_locked().await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            history::capture(
                &bottle.bottle_path(),
                message,
                true,
                Stage::Committing,
                &cx,
                &progress,
            )
            .await
        })
    }

    /// Lists caller-visible snapshots for this bottle.
    ///
    /// Results are newest-first. Every commit whose message is exactly
    /// `bottles-next:auto-checkpoint` is excluded because that value is reserved
    /// for internal mutation checkpoints.
    ///
    /// Listing serializes with runtime control, edits and deletion.
    /// A bottle without history returns an empty list without contacting FVS.
    ///
    /// # Errors
    ///
    /// Returns an error if the bottle was deleted, the FVS service is
    /// unavailable, or its snapshot history cannot be read.
    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        let _read = self.0.control.lock().await;
        self.ensure_exists()?;
        if !crate::utils::exists(&self.bottle_path().join(".fvs2")).await? {
            return Ok(Vec::new());
        }
        Ok(self
            .0
            .cx
            .fvs()
            .await?
            .list_commits(&history::repository(&self.bottle_path()))
            .await?
            .into_iter()
            .filter(|snapshot| snapshot.message != AUTO_CHECKPOINT_MESSAGE)
            .collect())
    }

    /// Restores the bottle to a snapshot selected by full state ID or prefix.
    ///
    /// The operation takes exclusive bottle access. It stops the bottle, then
    /// replaces the complete bottle tree with the target;
    /// files absent from that snapshot are removed. A checkpoint protects the
    /// current state if restore or metadata validation fails. The returned string is the resolved
    /// full state ID and the restored `bottle.toml` is published as a new
    /// [`BottleState`] snapshot.
    ///
    /// Currently this restores the working files without moving FVS's current
    /// commit to the target. Cancellation is observed before restore begins,
    /// but not while the FVS stream is running.
    ///
    /// A failed restore or invalid metadata restores the previous files and
    /// configuration before returning. Failed recovery reports the owner path.
    ///
    /// # Errors
    ///
    /// The operation returns an error if the bottle cannot be stopped, the
    /// target is missing or ambiguous, the restore fails, cancellation is
    /// requested, or the restored metadata has a different bottle UUID.
    pub fn rollback(&self, state_id_or_prefix: &str) -> Operation<String> {
        let bottle = self.clone();
        let bottle_path = self.bottle_path();
        let cx = self.0.cx.clone();
        let state_id_or_prefix = state_id_or_prefix.to_owned();
        Operation::new(move |progress, cancellation| async move {
            let _control = cancellation
                .run_until_cancelled(bottle.0.control.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            bottle.stop_locked().await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }

            let checkpoint = history::capture(
                &bottle_path,
                AUTO_CHECKPOINT_MESSAGE.into(),
                false,
                Stage::Checkpointing,
                &cx,
                &progress,
            )
            .await?;
            let result = async {
                let response =
                    history::restore(&bottle_path, &state_id_or_prefix, &cx, &progress).await?;
                let state: BottleState = next_config::load(bottle_path.join("bottle.toml")).await?;
                if state.id != bottle.0.id {
                    return Err(BottleError::IdMismatch {
                        expected: bottle.0.id,
                        actual: state.id,
                    }
                    .into());
                }
                state.environment.validate_requirements()?;
                Ok((response.state_id, state))
            }
            .await;
            let (revision, state) =
                history::recover(result, &bottle_path, &checkpoint, &cx, &progress).await?;
            bottle.publish(state);
            Ok(revision)
        })
    }
}
