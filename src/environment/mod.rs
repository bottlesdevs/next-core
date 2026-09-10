//! Shared execution configuration and the private runtime retained by an owner.

mod config;
mod error;
pub(crate) mod prefix;
mod software;

use std::path::PathBuf;

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

/// A private owner-cached handle. Dropping it only releases local resources.
/// Owners serialize access and persist configuration, including storage metadata.
pub(crate) struct Environment {
    pub(crate) config: EnvironmentConfig,
    root: PathBuf,
    cx: Context,
    runner: Option<Box<dyn Runner>>,
    bridge: Option<WineBridgeClient>,
}

impl Environment {
    pub(crate) fn new(config: EnvironmentConfig, root: PathBuf, cx: Context) -> Self {
        Self {
            config,
            root,
            cx,
            runner: None,
            bridge: None,
        }
    }

    async fn load_runner(&mut self) -> Result<()> {
        if self.runner.is_none() {
            self.runner = Some(
                self.config
                    .runner()
                    .load_runner(self.cx.directories(), self.config.umu())
                    .await?,
            );
        }
        Ok(())
    }

    async fn bridge(&mut self) -> Result<&WineBridgeClient> {
        if self.bridge.is_none() {
            self.load_runner().await?;
            prefix::prepare(&self.config.storage, &self.root, &self.cx).await?;
            let prefix = self.root.join("prefix");
            let command = self.config.wrappers.apply(
                WineBridgeClient::command(
                    self.runner.as_deref().expect("runner loaded"),
                    &prefix,
                    self.config.winebridge().path(self.cx.directories()),
                )
                .envs(self.config.env_vars.iter()),
            );
            self.bridge = Some(WineBridgeClient::connect_or_spawn(&prefix, command).await?);
        }
        Ok(self.bridge.as_ref().expect("bridge connected"))
    }

    pub(crate) async fn launch(
        &mut self,
        program: &ProgramSpec,
        cancellation: &CancellationToken,
    ) -> Result<u32> {
        let bridge = self.bridge().await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        bridge
            .launch_process(
                program.id(),
                program.executable().to_owned(),
                program.args().to_vec(),
                program.working_directory().map(str::to_owned),
                program.new_console(),
            )
            .await
    }

    pub(crate) async fn processes(&mut self) -> Result<Vec<Process>> {
        self.bridge().await?.list_processes().await
    }

    pub(crate) async fn kill(&mut self, id: uuid::Uuid) -> Result<()> {
        self.bridge().await?.kill_process(id).await
    }

    pub(crate) async fn dll_overrides(&mut self) -> Result<Vec<DllOverride>> {
        match self.bridge().await?.list_dll_overrides().await {
            Ok(overrides) => Ok(overrides),
            Err(Error::Status(status)) if status.code() == tonic::Code::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn set_dll_override(
        &mut self,
        dll: String,
        mode: DllOverrideMode,
    ) -> Result<()> {
        if mode == DllOverrideMode::Unspecified {
            return Err(EnvironmentError::DllOverrideModeRequired.into());
        }
        self.bridge().await?.set_dll_override(dll, mode).await
    }

    pub(crate) async fn unset_dll_override(&mut self, dll: String) -> Result<()> {
        match self.bridge().await?.delete_dll_override(dll).await {
            Err(Error::Status(status)) if status.code() == tonic::Code::NotFound => Ok(()),
            result => result,
        }
    }

    /// Attempts every cleanup action, retaining the first error for explicit retry.
    pub(crate) async fn stop(&mut self) -> Result<()> {
        let prefix = self.root.join("prefix");
        let runner_loaded = self.load_runner().await;
        let mut first_error = None;
        // Discovery also supports stopping a runtime left by a previous client.
        match WineBridgeClient::try_connect(&prefix).await {
            Ok(Some(bridge)) => {
                if let Err(error) = bridge.shutdown().await {
                    first_error.get_or_insert(error);
                }
            }
            Ok(None) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
        match runner_loaded {
            Ok(()) => {
                if let Err(error) =
                    shutdown_prefix(self.runner.as_deref().expect("runner loaded"), &prefix).await
                {
                    first_error.get_or_insert(error);
                }
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = prefix::stop(&self.config.storage, &self.root, &self.cx).await {
            first_error.get_or_insert(error);
        }
        first_error.map_or(Ok(()), Err)?;
        self.bridge = None;
        self.runner = None;
        Ok(())
    }
}
