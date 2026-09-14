//! Builds each addon against pinned Soda alone, without owner settings or private data.

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{VirgoLayer, VirgoManager, cache};
use crate::{
    AddonError, EnvironmentError, Progress, Slot, Stage,
    addons::{InstallInputs, execute},
    error::{Error, Result},
};

impl VirgoManager {
    pub(super) async fn prepare_addon(
        &self,
        id: Uuid,
        base: &VirgoLayer,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
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
        let component = self.addons.component(id);
        let dependency = self.addons.dependency(id);
        let (payload, resources) = if let Some(release) = &component {
            let payload = release.path(self.cx.directories());
            release.validate(&payload).await?;
            (payload, release.resources())
        } else if let Some(release) = &dependency {
            let payload = release.path(self.cx.directories());
            release.validate(&payload).await?;
            (payload, release.resources())
        } else {
            return Err(AddonError::NotFound(id).into());
        };
        let soda = self
            .addons
            .component(base.id)
            .ok_or(AddonError::NotFound(base.id))?;
        let runner = soda.load_runner(self.cx.directories(), None).await?;
        let winebridge = self
            .addons
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
