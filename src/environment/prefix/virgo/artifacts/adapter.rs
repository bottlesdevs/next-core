//! Runner initialization effects over pinned Soda, with registry changes stored as patches.

use tokio_util::sync::CancellationToken;

use super::{Reservation, VirgoLayer, VirgoManager, build};
use crate::{EnvironmentState, error::Result};

impl VirgoManager {
    pub(super) async fn prepare_adapter(
        &self,
        config: &EnvironmentState,
        base: &VirgoLayer,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let id = config.runner().id();
        let destination = std::path::Path::new("adapters").join(id.to_string());
        let build = match self
            .layers
            .reserve(&destination, Some(id), cancellation)
            .await?
        {
            Reservation::Cached(layer) => return Ok(layer),
            Reservation::Build(build) => build,
        };
        let runner = config
            .runner()
            .load_runner(self.cx.directories(), config.umu())
            .await?;
        build::run(
            build,
            id,
            id.to_string(),
            runner.as_ref(),
            Some(base),
            cancellation,
            |prefix, runner| async move { runner.wineboot(&prefix, "--init").await },
        )
        .await
    }
}
