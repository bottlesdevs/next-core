//! Running execution environments and lifecycle workflows driven by owner-held configuration.
//! Owners serialize access and persist state; prefix backends materialize execution settings.

mod config;
mod edit;
mod error;
pub use edit::Edit;
pub(crate) use edit::EnvironmentOwnerState;
#[cfg(feature = "fvs")]
pub(crate) mod history;
mod prefix;
mod runtime;

use crate::{
    Addons, Context, LaunchSpec, Progress, Stage,
    error::{Error, Result},
    proto::Process,
    winebridge::WineBridgeClient,
};
use std::path::Path;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub use config::EnvironmentState;
pub use error::EnvironmentError;
pub use prefix::PrefixBackend;
#[cfg(feature = "fvs")]
pub use prefix::VirgoError;
#[cfg(feature = "fvs")]
pub(crate) use prefix::VirgoManager;

/// Connect to a running environment without starting Wine or preparing storage.
/// Missing discovery returns `None`; malformed or
/// unreachable discovery retains the underlying bridge error.
pub(crate) async fn try_attach(root: &Path) -> Result<Option<WineBridgeClient>> {
    WineBridgeClient::try_connect(&root.join("prefix")).await
}

/// Attach after an application restart or prepare and start a stopped runtime.
/// A live attachment bypasses addon resolution, runner loading, and prefix preparation.
pub(crate) async fn attach_or_start(
    backend: &PrefixBackend,
    config: &EnvironmentState,
    root: &Path,
    cx: &Context,
    #[cfg(feature = "fvs")] virgo: &VirgoManager,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<WineBridgeClient> {
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if let Some(environment) = try_attach(root).await? {
        return Ok(environment);
    }
    let env_vars = config.effective_env_vars();
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    stop(backend, config, root, cx).await?;
    let runner = config
        .runner()
        .load_runner(cx.directories(), config.umu())
        .await?;
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let result = async {
        backend
            .prepare(
                config,
                runner.as_ref(),
                root,
                cx,
                #[cfg(feature = "fvs")]
                virgo,
                progress,
                cancellation,
            )
            .await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let prefix = root.join("prefix");
        let command = config.wrappers.apply(WineBridgeClient::command(
            runner.as_ref(),
            &prefix,
            config.winebridge().path(cx.directories()),
            env_vars.iter(),
        ));
        let bridge = WineBridgeClient::connect_or_spawn(&prefix, command).await?;
        Ok(bridge)
    }
    .await;
    runtime::finish_start(result, cancellation, async {
        runtime::stop(runner.as_ref(), &root.join("prefix")).await?;
        backend.release(root, cx).await
    })
    .await
}

pub(crate) async fn launch(
    bridge: &WineBridgeClient,
    id: Uuid,
    program: &LaunchSpec,
) -> Result<u32> {
    bridge
        .launch_process(
            id,
            program.executable().to_owned(),
            program.args().to_vec(),
            program.working_directory().map(str::to_owned),
            program.new_console(),
        )
        .await
}

/// Initialize prefix data without returning a running environment.
/// Failed initialization may retain live storage; the owner must not remove its
/// directory unless this function succeeds.
pub(crate) async fn initialize(
    backend: &PrefixBackend,
    config: &EnvironmentState,
    root: &Path,
    cx: &Context,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    progress.send_replace(Some(Progress::new(Stage::CreatingPrefix)));
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    backend.create(config, root, cx).await
}

/// Inspect processes without starting Wine or preparing prefix storage.
pub(crate) async fn processes(root: &Path) -> Result<Vec<Process>> {
    match try_attach(root).await? {
        Some(environment) => environment.list_processes().await,
        None => Ok(Vec::new()),
    }
}

/// Terminate a UUID-keyed process group without starting a stopped runtime.
pub(crate) async fn kill(root: &Path, id: Uuid) -> Result<()> {
    if let Some(environment) = try_attach(root).await? {
        environment.kill_process(id).await?;
    }
    Ok(())
}

/// Stop Wine before releasing prefix storage, even when WineBridge is unreachable.
pub(crate) async fn stop(
    backend: &PrefixBackend,
    config: &EnvironmentState,
    root: &Path,
    cx: &Context,
) -> Result<()> {
    let prefix = root.join("prefix");
    // No runtime exists until the backend has materialized a prefix.
    if !crate::utils::exists(&prefix).await? {
        return Ok(());
    }
    let runner = config
        .runner()
        .load_runner(cx.directories(), config.umu())
        .await?;
    runtime::stop(runner.as_ref(), &prefix).await?;
    backend.release(root, cx).await
}

/// Apply a candidate to stopped prefix data; the owner persists and publishes it afterward.
pub(crate) async fn apply(
    backend: &PrefixBackend,
    previous: &EnvironmentState,
    candidate: &EnvironmentState,
    root: &Path,
    cx: &Context,
    addons: &Addons,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    candidate.validate_edit(previous, addons)?;
    backend.validate_edit(previous, candidate)?;
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if candidate == previous {
        return Ok(());
    }
    if try_attach(root).await?.is_some() {
        return Err(EnvironmentError::MustBeStopped.into());
    }
    stop(backend, previous, root, cx).await?;
    backend
        .apply(
            previous,
            candidate,
            root,
            cx,
            addons,
            progress,
            cancellation,
        )
        .await
}
