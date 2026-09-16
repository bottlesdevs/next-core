//! Builds each addon against pinned Soda alone, without owner settings or private data.

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{Reservation, VirgoLayer, VirgoManager, build, latest_component};
use crate::{
    Addon, AddonError, EnvironmentError, Progress, Slot, Stage,
    addons::{AddonFamily, InstallInputs, execute},
    error::Result,
};

impl VirgoManager {
    /// Reuse cached effects or execute the selected frozen recipe in an isolated build.
    /// Acquisition validates payloads; missing inputs fail when the recipe uses them.
    pub(super) async fn prepare_addon<K: AddonFamily>(
        &self,
        addon: &Addon<K>,
        base: &VirgoLayer,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let id = addon.id();
        let destination = std::path::Path::new("addons").join(id.to_string());
        let build = match self
            .layers
            .reserve(&destination, Some(id), cancellation)
            .await?
        {
            Reservation::Cached(layer) => return Ok(layer),
            Reservation::Build(build) => build,
        };
        let payload = addon.path(self.cx.directories());
        let resources = addon.resources();
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
        build::run(
            build,
            id,
            id.to_string(),
            runner.as_ref(),
            Some(base),
            cancellation,
            |prefix, runner| async move {
                execute(
                    InstallInputs {
                        prefix: &prefix,
                        runner,
                        winebridge: &winebridge,
                    },
                    &payload,
                    resources,
                    false,
                    cancellation,
                    |_| {
                        progress.send_replace(Some(Progress::new(Stage::Configuring)));
                    },
                )
                .await
            },
        )
        .await
    }
}
