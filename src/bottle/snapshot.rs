use std::path::Path;

use fvs_rs::{Repository, RestoreResponse};

use crate::error::Result;

use super::{
    Bottle, FVS_BLOCK_SIZE, Snapshot, SnapshotSummary, bottle::BottleConfig, error::BottleError,
};

impl Bottle {
    pub async fn create_snapshot(&mut self, message: impl Into<String>) -> Result<Snapshot> {
        self.stop().await?;
        Ok(self
            .context
            .fvs()
            .await?
            .commit(&self.snapshot_repository(), message.into())
            .await?)
    }

    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        Ok(self
            .context
            .fvs()
            .await?
            .list_commits(&self.snapshot_repository())
            .await?)
    }

    pub async fn rollback(&mut self, state_id_or_prefix: &str) -> Result<String> {
        self.stop().await?;
        let id = self.id();
        let response: RestoreResponse = self
            .context
            .fvs()
            .await?
            .restore(
                &self.snapshot_repository(),
                state_id_or_prefix,
                None::<&Path>,
                true,
                false,
            )
            .await?;
        let config: BottleConfig = next_config::load(self.bottle_path().join("bottle.toml"))?;
        if config.id != id {
            return Err(BottleError::IdMismatch {
                expected: id,
                actual: config.id,
            }
            .into());
        }
        self.config = config;
        Ok(response.state_id)
    }

    fn snapshot_repository(&self) -> Repository {
        Repository {
            repository_path: self.bottle_path().display().to_string(),
            block_size: FVS_BLOCK_SIZE,
        }
    }
}
