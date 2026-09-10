//! Shared execution configuration and the private runtime retained by an owner.

mod config;
mod error;
pub(crate) mod prefix;
mod software;

pub(crate) use software::reconcile;

use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::{
    Context, ProgramSpec,
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
    runner::{Runner, shutdown_prefix},
    winebridge::WineBridgeClient,
};

pub use config::EnvironmentConfig;
pub use error::EnvironmentError;
pub use prefix::Storage;

/// A private owner-cached live runtime. Construction connects WineBridge.
/// Dropping it only releases local resources.
/// Owners serialize access and persist configuration, including storage metadata.
pub(crate) struct Environment {
    // Retain the resolved runner with the live handle; shutdown uses saved settings.
    #[allow(dead_code)]
    runner: Box<dyn Runner>,
    bridge: WineBridgeClient,
}

impl Environment {
    /// Connects to an existing runtime or prepares and starts one.
    pub(crate) async fn attach_or_start(
        config: &EnvironmentConfig,
        root: PathBuf,
        cx: Context,
    ) -> Result<Self> {
        let runner = config
            .runner()
            .load_runner(cx.directories(), config.umu())
            .await?;
        let prefix = root.join("prefix");
        if let Some(bridge) = WineBridgeClient::try_connect(&prefix).await? {
            return Ok(Self { runner, bridge });
        }
        prefix::prepare(&config.storage, &root, &cx).await?;
        let command = config.wrappers.apply(
            WineBridgeClient::command(
                runner.as_ref(),
                &prefix,
                config.winebridge().path(cx.directories()),
            )
            .envs(config.env_vars.iter()),
        );
        let bridge = WineBridgeClient::connect_or_spawn(&prefix, command).await?;
        Ok(Self { runner, bridge })
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

    /// Stops Wine and releases storage without requiring a live handle or bridge.
    pub(crate) async fn stop(config: &EnvironmentConfig, root: &Path, cx: &Context) -> Result<()> {
        let runner = config
            .runner()
            .load_runner(cx.directories(), config.umu())
            .await?;
        let prefix = root.join("prefix");
        match WineBridgeClient::try_connect(&prefix).await {
            Ok(Some(bridge)) => {
                if let Err(error) = bridge.shutdown().await {
                    tracing::debug!(%error, "WineBridge shutdown failed; stopping wineserver");
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, "WineBridge discovery failed; stopping wineserver");
            }
        }
        shutdown_prefix(runner.as_ref(), &prefix).await?;
        prefix::stop(&config.storage, root, cx).await
    }
}
