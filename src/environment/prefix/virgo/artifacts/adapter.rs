//! Runner initialization effects over pinned Soda, with registry changes stored as patches.

use tokio_util::sync::CancellationToken;

use super::{VirgoLayer, VirgoManager, cache};
use crate::{
    EnvironmentState,
    error::{Error, Result},
};

impl VirgoManager {
    pub(super) async fn prepare_adapter(
        &self,
        config: &EnvironmentState,
        base: &VirgoLayer,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let id = config.runner().id();
        let data_dir = self.cx.directories().data_dir();
        let destination = data_dir.join("virgo/adapters").join(id.to_string());
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
        let runner = config
            .runner()
            .load_runner(self.cx.directories(), config.umu())
            .await?;
        self.build(
            id,
            &destination,
            runner.as_ref(),
            base,
            cancellation,
            |prefix, runner| async move { runner.wineboot(&prefix, "--init").await },
        )
        .await
    }
}
