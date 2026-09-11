//! Builds each addon against pinned Soda alone, without owner settings or private data.

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{cache, downloaded_soda, ensure_base};
use crate::{
    AddonError, Addons, Context, EnvVars, EnvironmentError, Progress, Slot, Stage,
    addons::{InstallInputs, execute},
    error::{Error, Result},
};

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
    let component = addons.component(id);
    let dependency = addons.dependency(id);
    let (payload, resources) = if let Some(release) = &component {
        release.require_payload(cx.directories()).await?;
        (release.path(cx.directories()), release.resources())
    } else if let Some(release) = &dependency {
        release.require_payload(cx.directories()).await?;
        (release.path(cx.directories()), release.resources())
    } else {
        return Err(AddonError::NotFound(id).into());
    };
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
                &payload,
                resources,
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
