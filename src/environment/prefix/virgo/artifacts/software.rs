//! Builds each addon against pinned Soda alone, without owner settings or private data.

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{VirgoLayer, VirgoManager, cache};
use crate::{
    AddonError, Addons, Context, EnvVars, EnvironmentError, Progress, Slot, Stage,
    addons::{InstallInputs, execute},
    error::{Error, Result},
};

impl VirgoManager {
    pub(super) async fn prepare_addon(
        &self,
        id: Uuid,
        base: &VirgoLayer,
        addons: &Addons,
        cx: &Context,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let destination = self.root.join("addons").join(id.to_string());
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
        let component = addons.component(id);
        let dependency = addons.dependency(id);
        let (payload, resources) = if let Some(release) = &component {
            let payload = release.path(cx.directories());
            release.validate(&payload).await?;
            (payload, release.resources())
        } else if let Some(release) = &dependency {
            let payload = release.path(cx.directories());
            release.validate(&payload).await?;
            (payload, release.resources())
        } else {
            return Err(AddonError::NotFound(id).into());
        };
        let soda = addons
            .component(base.id)
            .ok_or(AddonError::NotFound(base.id))?;
        let runner = soda.addon().load_runner(cx.directories(), None).await?;
        let winebridge = addons
            .latest_component(Slot::WineBridge)
            .ok_or(EnvironmentError::ComponentNotInstalled(Slot::WineBridge))?
            .path(cx.directories());
        self.build(
            id,
            &destination,
            runner.as_ref(),
            base,
            cx,
            cancellation,
            |prefix, runner| async move {
                let mut env_vars = EnvVars::default();
                execute(
                    InstallInputs {
                        prefix: &prefix,
                        runner,
                        winebridge: &winebridge,
                        env_vars: &mut env_vars,
                        explicit_env_vars: &EnvVars::default(),
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
