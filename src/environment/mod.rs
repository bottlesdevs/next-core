//! Shared execution configuration and temporary connections to a running environment.

#[cfg(feature = "fvs")]
pub(crate) mod history;

mod config;
mod error;
pub(crate) mod prefix;
mod software;

pub(crate) use software::validate_edit;

use std::path::Path;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    Context, ProgramSpec, Progress,
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
    runner::Runner,
    winebridge::WineBridgeClient,
};

pub use config::EnvironmentConfig;
pub use error::EnvironmentError;
pub use prefix::Storage;

/// A private connection to a live runtime for one control operation.
/// Dropping it only releases local resources.
/// Owners serialize access and persist configuration, including storage metadata.
pub(crate) struct Environment {
    bridge: WineBridgeClient,
}

impl Environment {
    pub(crate) async fn initialize(runner: &dyn Runner, prefix: &Path) -> Result<()> {
        let initialized = runner.wineboot(prefix, "--init").await;
        shutdown_wine(runner, prefix).await?;
        initialized
    }

    /// Stops Wine and unmounts storage without requiring a live handle or bridge.
    pub(crate) async fn stop(config: &EnvironmentConfig, root: &Path, cx: &Context) -> Result<()> {
        let prefix = root.join("prefix");
        // A lazily created Virgo owner has no runtime mountpoint yet.
        if !crate::utils::exists(&prefix).await? {
            return Ok(());
        }
        let runner = config
            .runner()
            .load_runner(cx.directories(), config.umu())
            .await?;
        shutdown_wine(runner.as_ref(), &prefix).await?;
        prefix::stop(&config.storage, root, cx).await
    }

    /// Materializes pending selections, then mounts and starts the selected runtime.
    pub(crate) async fn start(
        config: &EnvironmentConfig,
        root: &Path,
        cx: &Context,
        addons: &crate::Addons,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<Self> {
        let env_vars = config.addon_env_vars(addons)?;
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
        prefix::prepare(config, root, cx, addons, progress, cancellation).await?;
        let prefix = root.join("prefix");
        let command = config.wrappers.apply(WineBridgeClient::command(
            runner.as_ref(),
            &prefix,
            config.winebridge().path(cx.directories()),
            env_vars.iter().chain(config.env_vars.iter()),
        ));
        let bridge = match WineBridgeClient::connect_or_spawn(&prefix, command).await {
            Ok(bridge) => bridge,
            Err(error) => {
                Self::stop(config, root, cx).await?;
                return Err(error);
            }
        };
        Ok(Self { bridge })
    }

    pub(crate) async fn launch_program(
        &self,
        program: &ProgramSpec,
        cancellation: &CancellationToken,
    ) -> Result<u32> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
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

    /// Attaches without starting Wine or mounting storage.
    pub(crate) async fn try_attach(root: &Path) -> Result<Option<Self>> {
        let Some(bridge) = WineBridgeClient::try_connect(&root.join("prefix")).await? else {
            return Ok(None);
        };
        Ok(Some(Self { bridge }))
    }

    pub(crate) async fn processes(&self) -> Result<Vec<Process>> {
        self.bridge.list_processes().await
    }

    pub(crate) async fn kill(&self, id: uuid::Uuid) -> Result<()> {
        self.bridge.kill_process(id).await
    }

    pub(crate) async fn dll_overrides(&self) -> Result<Vec<DllOverride>> {
        match self.bridge.list_dll_overrides().await {
            Ok(overrides) => Ok(overrides),
            Err(Error::Status(status)) if status.code() == tonic::Code::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn set_dll_override(&self, dll: String, mode: DllOverrideMode) -> Result<()> {
        self.bridge.set_dll_override(dll, mode).await
    }

    pub(crate) async fn unset_dll_override(&self, dll: String) -> Result<()> {
        match self.bridge.delete_dll_override(dll).await {
            Err(Error::Status(status)) if status.code() == tonic::Code::NotFound => Ok(()),
            result => result,
        }
    }
}

/// Stops WineBridge and waits for wineserver; the caller owns storage cleanup.
async fn shutdown_wine(runner: &dyn Runner, prefix: &Path) -> Result<()> {
    if let Err(error) = WineBridgeClient::shutdown_existing(prefix).await {
        tracing::debug!(%error, "WineBridge shutdown failed; stopping wineserver");
    }
    for argument in ["-k", "-w"] {
        runner
            .wineserver(prefix, argument)
            .await
            .map_err(|source| EnvironmentError::Cleanup {
                prefix: prefix.to_path_buf(),
                source: Box::new(source),
            })?;
    }
    if let Err(error) = WineBridgeClient::clear_discovery(prefix).await {
        tracing::warn!(%error, "could not remove WineBridge discovery after shutdown");
    }
    Ok(())
}
