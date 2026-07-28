//! Snapshot history operations.

use std::path::Path;

use fvs_rs::{Repository, RestoreResponse};

use crate::{
    Operation,
    error::{Error, Result},
    prefix::{
        CHECKPOINT_MESSAGE, FVS_BLOCK_SIZE, TransactionProgress, finish_commit, finish_restore,
    },
};

use super::{Bottle, Snapshot, SnapshotSummary, error::BottleError, state::BottleState};

impl Bottle {
    pub fn create_snapshot(
        &self,
        message: impl Into<String>,
    ) -> Operation<Snapshot, SnapshotProgress> {
        let bottle = self.clone();
        let repository = self.snapshot_repository();
        let cx = self.0.cx.clone();
        let message = message.into();
        self.0.cx.spawn(move |progress, cancellation| async move {
            progress.send_replace(Some(SnapshotProgress::Stopping));
            bottle.stop().await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let stream = cx.fvs().await?.commit_stream(&repository, message).await?;
            finish_commit(stream, |update| {
                progress.send_replace(Some(SnapshotProgress::Committing(update.into())));
            })
            .await
        })
    }

    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
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
            .filter(|snapshot| snapshot.message != CHECKPOINT_MESSAGE)
            .collect())
    }

    pub fn rollback(&self, state_id_or_prefix: &str) -> Operation<String, RollbackProgress> {
        let bottle = self.clone();
        let repository = self.snapshot_repository();
        let bottle_path = self.bottle_path();
        let cx = self.0.cx.clone();
        let runtime = self.0.cx.clone();
        let state_id_or_prefix = state_id_or_prefix.to_owned();
        runtime.spawn(move |progress, cancellation| async move {
            progress.send_replace(Some(RollbackProgress::Stopping));
            bottle.stop().await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }

            let _write = bottle.0.write.lock().await;
            bottle.ensure_exists()?;
            let stream = cx
                .fvs()
                .await?
                .restore_stream(&repository, &state_id_or_prefix, None::<&Path>, true, false)
                .await?;
            let response: RestoreResponse = finish_restore(stream, |update| {
                progress.send_replace(Some(RollbackProgress::Restoring(update.into())));
            })
            .await?;
            let path = bottle_path.join("bottle.toml");
            let state: BottleState = cx
                .spawn_blocking(move || Ok(next_config::load(path)?))
                .await?;
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

    fn snapshot_repository(&self) -> Repository {
        Repository {
            repository_path: self.bottle_path().display().to_string(),
            block_size: FVS_BLOCK_SIZE,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RollbackProgress {
    Stopping,
    Restoring(TransactionProgress),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotProgress {
    Stopping,
    Committing(TransactionProgress),
}
