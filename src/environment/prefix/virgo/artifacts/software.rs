//! Builds each addon against pinned Soda alone, without owner settings or private data.

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{VirgoLayer, VirgoManager, cache};
use crate::{
    Addon, AddonError, EnvironmentError, Progress, Slot, Stage,
    addons::{AddonFamily, InstallInputs, execute},
    error::{Error, Result},
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
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let id = addon.id();
        let data_dir = self.cx.directories().data_dir();
        let destination = data_dir.join("virgo/addons").join(id.to_string());
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
        let payload = addon.path(self.cx.directories());
        let resources = addon.resources();
        let soda = self
            .cx
            .addons()
            .component(base.id)
            .ok_or(AddonError::NotFound(base.id))?;
        let runner = soda.load_runner(self.cx.directories(), None).await?;
        let winebridge = self
            .cx
            .addons()
            .latest_component(Slot::WineBridge)
            .ok_or(EnvironmentError::ComponentNotInstalled(Slot::WineBridge))?
            .path(self.cx.directories());
        self.build(
            id,
            &destination,
            runner.as_ref(),
            base,
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
