//! Running execution environments and lifecycle workflows driven by owner-held configuration.
//! Owners serialize access and persist state; prefix backends materialize execution settings.

mod config;
mod error;
#[cfg(feature = "fvs")]
pub(crate) mod history;
mod prefix;
mod runtime;

use crate::{
    Addons, Context, ProgramSpec, Progress, Stage,
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
    winebridge::WineBridgeClient,
};
use std::path::Path;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub use config::EnvironmentConfig;
pub use error::EnvironmentError;
pub use prefix::PrefixBackend;
#[cfg(feature = "fvs")]
pub use prefix::VirgoError;

/// A temporary connection to a running execution environment.
/// Successful construction establishes a WineBridge connection. The runtime may
/// later exit; operations then return connection errors. Dropping this handle
/// releases only the connection, and the owner retains configuration and locking.
pub(crate) struct Environment {
    bridge: WineBridgeClient,
}

impl Environment {
    /// Connect to a running environment without starting Wine or preparing storage.
    /// Missing discovery returns `None`; malformed or
    /// unreachable discovery retains the underlying bridge error.
    pub(crate) async fn try_attach(root: &Path) -> Result<Option<Self>> {
        Ok(WineBridgeClient::try_connect(&root.join("prefix"))
            .await?
            .map(|bridge| Self { bridge }))
    }

    /// Attach after an application restart or prepare and start a stopped runtime.
    /// A live attachment bypasses addon resolution, runner loading, and prefix preparation.
    pub(crate) async fn attach_or_start(
        config: &EnvironmentConfig,
        root: &Path,
        cx: &Context,
        addons: &Addons,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<Self> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if let Some(environment) = Self::try_attach(root).await? {
            return Ok(environment);
        }
        let env_vars = config.addon_env_vars();
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Self::stop(config, root, cx).await?;
        let runner = config
            .runner()
            .load_runner(cx.directories(), config.umu())
            .await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        config
            .backend
            .prepare(
                config,
                runner.as_ref(),
                root,
                cx,
                addons,
                progress,
                cancellation,
            )
            .await?;
        let prefix = root.join("prefix");
        let command = config.wrappers.apply(WineBridgeClient::command(
            runner.as_ref(),
            &prefix,
            config.winebridge().path(cx.directories()),
            env_vars.iter().chain(config.env_vars.iter()),
        ));
        match WineBridgeClient::connect_or_spawn(&prefix, command).await {
            Ok(bridge) => Ok(Self { bridge }),
            Err(error) => {
                runtime::stop(runner.as_ref(), &prefix).await?;
                config.backend.release(root, cx).await?;
                Err(error)
            }
        }
    }

    pub(crate) async fn launch(&self, program: &ProgramSpec) -> Result<u32> {
        self.bridge
            .launch_process(
                program.id(),
                program.executable().to_owned(),
                program.args().to_vec(),
                program.working_directory().map(str::to_owned),
                program.new_console(),
            )
            .await
    }

    pub(crate) async fn dll_overrides(&self) -> Result<Vec<DllOverride>> {
        self.bridge.list_dll_overrides().await
    }

    pub(crate) async fn set_dll_override(&self, dll: String, mode: DllOverrideMode) -> Result<()> {
        self.bridge.set_dll_override(dll, mode).await
    }

    pub(crate) async fn unset_dll_override(&self, dll: String) -> Result<()> {
        self.bridge.delete_dll_override(dll).await
    }

    /// Initialize prefix data without returning a running environment.
    /// Failed initialization may retain live storage; the owner must not remove its
    /// directory unless this function succeeds.
    pub(crate) async fn initialize(
        config: &EnvironmentConfig,
        root: &Path,
        cx: &Context,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        progress.send_replace(Some(Progress::new(Stage::CreatingPrefix)));
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        config.backend.create(config, root, cx).await
    }

    /// Inspect processes without starting Wine or preparing prefix storage.
    pub(crate) async fn processes(root: &Path) -> Result<Vec<Process>> {
        match Self::try_attach(root).await? {
            Some(environment) => environment.bridge.list_processes().await,
            None => Ok(Vec::new()),
        }
    }

    /// Terminate a UUID-keyed process group without starting a stopped runtime.
    pub(crate) async fn kill(root: &Path, id: Uuid) -> Result<()> {
        if let Some(environment) = Self::try_attach(root).await? {
            environment.bridge.kill_process(id).await?;
        }
        Ok(())
    }

    /// Stop Wine before releasing prefix storage, even when WineBridge is unreachable.
    pub(crate) async fn stop(config: &EnvironmentConfig, root: &Path, cx: &Context) -> Result<()> {
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
        config.backend.release(root, cx).await
    }

    /// Apply a candidate to stopped prefix data; the owner persists and publishes it afterward.
    pub(crate) async fn apply(
        previous: &EnvironmentConfig,
        candidate: &EnvironmentConfig,
        root: &Path,
        cx: &Context,
        addons: &Addons,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        candidate.validate_edit(previous, addons)?;
        candidate.backend.validate_edit(previous, candidate)?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if candidate == previous {
            return Ok(());
        }
        if Self::try_attach(root).await?.is_some() {
            return Err(EnvironmentError::MustBeStopped.into());
        }
        Self::stop(previous, root, cx).await?;
        candidate
            .backend
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
}
