//! Builds each addon against pinned Soda alone, without owner settings or private data.

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{VirgoLayer, VirgoManager, latest_component};
use crate::{
    Addon, AddonError, EnvironmentError, Progress, Slot, Stage,
    addons::{AddonFamily, InstallInputs, execute},
    environment::runtime,
    error::Result,
};

impl VirgoManager {
    /// Reuse cached effects or execute the selected frozen recipe in an isolated build.
    pub(super) async fn prepare_addon<K: AddonFamily>(
        &self,
        addon: &Addon<K>,
        base: &VirgoLayer,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let id = addon.id();
        let destination = std::path::Path::new("addons").join(id.to_string());
        self.layers
            .get_or_build(&destination, Some(id), cancellation, || async {
                let soda = self
                    .cx
                    .addons()
                    .component(base.id)
                    .ok_or(AddonError::NotFound(base.id))?;
                let runner = soda.load_runner(self.cx.directories(), None).await?;
                let winebridge = latest_component(
                    self.cx
                        .addons()
                        .components()
                        .into_iter()
                        .filter(|addon| addon.slot() == Slot::WineBridge),
                )
                .ok_or(EnvironmentError::ComponentNotInstalled(Slot::WineBridge))?
                .path(self.cx.directories());
                let workspace = self
                    .layers
                    .prepare_build(&destination, Some(base), cancellation)
                    .await?;
                let executed = execute(
                    InstallInputs {
                        prefix: &workspace.prefix,
                        runner: runner.as_ref(),
                        winebridge: &winebridge,
                    },
                    &addon.path(self.cx.directories()),
                    addon.resources(),
                    false,
                    cancellation,
                    |_| {
                        progress.send_replace(Some(Progress::new(Stage::Configuring)));
                    },
                )
                .await;
                runtime::stop(runner.as_ref(), &workspace.prefix).await?;
                self.layers
                    .finish_build(workspace, id, id.to_string(), executed, cancellation)
                    .await
            })
            .await
    }
}
