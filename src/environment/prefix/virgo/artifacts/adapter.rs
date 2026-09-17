//! Runner initialization effects over pinned Soda, with registry changes stored as patches.

use tokio_util::sync::CancellationToken;

use super::{VirgoLayer, VirgoManager};
use crate::{
    EnvironmentState,
    environment::runtime,
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
        let destination = std::path::Path::new("adapters").join(id.to_string());
        self.layers
            .get_or_build(&destination, Some(id), cancellation, || async {
                let runner = config
                    .runner()
                    .load_runner(self.cx.directories(), config.umu())
                    .await?;
                let workspace = self
                    .layers
                    .prepare_build(&destination, Some(base), cancellation)
                    .await?;
                let executed = if cancellation.is_cancelled() {
                    Err(Error::Cancelled)
                } else {
                    runner.wineboot(&workspace.prefix, "--init").await
                };
                runtime::stop(runner.as_ref(), &workspace.prefix).await?;
                self.layers
                    .finish_build(workspace, id, id.to_string(), executed, cancellation)
                    .await
            })
            .await
    }
}
