//! Builds each addon against pinned Soda alone, without owner settings or private data.

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{cache, downloaded_soda, ensure_base};
use crate::{
    AddonError, Addons, Context, EnvVars, EnvironmentError, Progress, Slot, Stage,
    addons::{Artifact, InstallInputs, execute},
    error::{Error, Result},
};

fn resources(id: Uuid, addons: &Addons, cx: &Context) -> Result<Vec<Artifact>> {
    if let Some(component) = addons.component(id) {
        return Ok(vec![component.artifact(cx.directories())]);
    }
    let dependency = addons.dependency(id).ok_or(AddonError::NotFound(id))?;
    Ok(dependency.resources(cx.directories()))
}

pub(crate) async fn prepare_addon(
    id: Uuid,
    addons: &Addons,
    cx: &Context,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    let _build = cancellation
        .run_until_cancelled(cx.artifact_build().lock())
        .await
        .ok_or(Error::Cancelled)?;
    // UUID alone is the cache identity, independent of owner settings and runner.
    if cache::exists(id, cx).await? {
        return Ok(());
    }
    let base = ensure_base(addons, cx, cancellation).await?;
    let soda = downloaded_soda(base.soda.id(), base.soda.version(), addons)?;
    let runner = soda.addon().load_runner(cx.directories(), None).await?;
    let winebridge = addons
        .latest_component(Slot::WineBridge)
        .ok_or(EnvironmentError::ComponentNotInstalled(Slot::WineBridge))?
        .path(cx.directories());
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut env_vars = EnvVars::default();
    let resources = resources(id, addons, cx)?;
    cache::install(
        base.layer,
        id,
        runner.as_ref(),
        async |prefix| {
            execute(
                InstallInputs {
                    prefix,
                    runner: runner.as_ref(),
                    winebridge: &winebridge,
                    env_vars: &mut env_vars,
                    explicit_env_vars: &EnvVars::default(),
                },
                &resources,
                cancellation,
                |_| {
                    progress.send_replace(Some(Progress::new(Stage::Configuring)));
                },
            )
            .await
        },
        cx,
    )
    .await
}
