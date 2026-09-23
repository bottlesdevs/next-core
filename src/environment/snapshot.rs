//! Owner-root history, serialized by the same lock as edits and runtime work.

use super::{
    BackendSource, Environment, State,
    history::{self, AUTO_CHECKPOINT_MESSAGE},
};
use crate::{
    EnvironmentError, Operation, Progress, Stage,
    error::{Error, Result},
};
pub use fvs_rs::{Commit as Snapshot, CommitSummary as SnapshotSummary};
use std::sync::Arc;

impl<T: BackendSource> Environment<T>
where
    State<T>: next_config::Config + Clone + PartialEq + Send + Sync,
{
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
            history::capture(
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

    pub(crate) async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        let _control = self.control.lock().await;
        self.state()?;
        if !crate::utils::exists(&self.root.join(".fvs2")).await? {
            return Ok(Vec::new());
        }
        Ok(self
            .context
            .fvs()
            .list_commits(&history::repository(&self.root))
            .await?
            .into_iter()
            .filter(|snapshot| snapshot.message != AUTO_CHECKPOINT_MESSAGE)
            .collect())
    }

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
            let checkpoint = history::capture(
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
                let restored = history::restore(
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
            let (revision, state) = history::recover(
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
