//! Runner initialization effects over pinned Soda, with registry changes stored as patches.

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{VirgoLayer, VirgoManager, cache};
use crate::{
    error::{Error, Result},
    runner::Runner,
};

impl VirgoManager {
    pub(super) async fn prepare_adapter(
        &self,
        id: Uuid,
        runner: &dyn Runner,
        base: &VirgoLayer,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let destination = self.root().join("adapters").join(id.to_string());
        if let Some(artifact) = cache::load(&destination, Some(id)).await? {
            return Ok(artifact);
        }
        let _build = cancellation
            .run_until_cancelled(self.build_lock.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if let Some(artifact) = cache::load(&destination, Some(id)).await? {
            return Ok(artifact);
        }
        self.build(
            id,
            &destination,
            runner,
            base,
            cancellation,
            |prefix, runner| async move { runner.wineboot(&prefix, "--init").await },
        )
        .await
    }
}
