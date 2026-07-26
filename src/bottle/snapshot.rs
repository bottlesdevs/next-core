use std::path::Path;

use fvs_rs::{Repository, RestoreResponse};

use crate::{
    Operation,
    error::{Error, Result},
};

use super::{
    Bottle, FVS_BLOCK_SIZE, Snapshot, SnapshotSummary, bottle::BottleState, error::BottleError,
};

impl Bottle {
    pub async fn create_snapshot(&self, message: impl Into<String>) -> Result<Snapshot> {
        let stop = self.stop();
        let repository = self.snapshot_repository();
        let cx = self.cx.clone();
        let message = message.into();
        let runtime = self.cx.clone();
        let operation: Operation<_, ()> = runtime.spawn(move |_, _| async move {
            stop.await?;
            Ok(cx.fvs().await?.commit(&repository, message).await?)
        });
        operation.await
    }

    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        let repository = self.snapshot_repository();
        let cx = self.cx.clone();
        let runtime = self.cx.clone();
        let operation: Operation<_, ()> = runtime
            .spawn(move |_, _| async move { Ok(cx.fvs().await?.list_commits(&repository).await?) });
        operation.await
    }

    pub fn rollback(&self, state_id_or_prefix: &str) -> Operation<String, RollbackProgress> {
        let stop = self.stop();
        let id = self.id;
        let repository = self.snapshot_repository();
        let bottle_path = self.bottle_path();
        let current_state = self.state.clone();
        let cx = self.cx.clone();
        let runtime = self.cx.clone();
        let state_id_or_prefix = state_id_or_prefix.to_owned();
        runtime.spawn(move |progress, cancellation| async move {
            progress.send_replace(Some(RollbackProgress::Stopping));
            stop.await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }

            progress.send_replace(Some(RollbackProgress::Restoring));
            let mut current_state = current_state.lock().await;
            let response: RestoreResponse = cx
                .fvs()
                .await?
                .restore(&repository, &state_id_or_prefix, None::<&Path>, true, false)
                .await?;
            let path = bottle_path.join("bottle.toml");
            let state: BottleState = cx
                .spawn_blocking(move || Ok(next_config::load(path)?))
                .await?;
            if state.id != id {
                return Err(BottleError::IdMismatch {
                    expected: id,
                    actual: state.id,
                }
                .into());
            }
            *current_state = state;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RollbackProgress {
    Stopping,
    Restoring,
}
