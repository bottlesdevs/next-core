mod proton;
mod wine;

pub(crate) use crate::wrapper::{Command, Spawnable, Wrapper};
use async_trait::async_trait;
pub use proton::Proton;
use thiserror::Error;
pub use wine::Wine;

use crate::{compatibility::components::catalog::RunnerKind, error::Result};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::ExitStatus,
};

/// Errors produced by runner setup.
#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("wineboot exited unsuccessfully: {0}")]
    WinebootFailed(ExitStatus),
    #[error("wineserver exited unsuccessfully: {0}")]
    WineserverFailed(ExitStatus),
    #[error("Proton runner requires an UMU executable")]
    UmuExecutableMissing,
    #[error("no supported runner executable was found in {0}")]
    RunnerNotFound(PathBuf),
    #[error("runner executable was not found: {0}")]
    RunnerExecutableNotFound(PathBuf),
}

#[derive(Debug)]
pub(crate) struct RunnerCommand(Command);

impl RunnerCommand {
    pub(crate) fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.0 = self.0.env(key, value);
        self
    }

    pub(crate) fn envs<K: AsRef<OsStr>, V: AsRef<OsStr>>(
        mut self,
        envs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.0 = self.0.envs(envs);
        self
    }
}

impl From<RunnerCommand> for Command {
    fn from(command: RunnerCommand) -> Self {
        command.0
    }
}

impl Spawnable for RunnerCommand {}

#[async_trait]
pub(crate) trait Runner: Send + Sync {
    fn command(&self, prefix: &Path, inner: Command) -> RunnerCommand;

    async fn wineboot(&self, prefix: &Path, arg: &str) -> Result<()> {
        let status = self
            .command(prefix, Command::new("wineboot").arg(arg))
            .spawn()?
            .wait()
            .await?;

        if !status.success() {
            return Err(RunnerError::WinebootFailed(status).into());
        }

        Ok(())
    }

    async fn wineserver(&self, prefix: &Path, arg: &str) -> Result<()>;
}

pub(crate) async fn initialize_and_shutdown_prefix(
    runner: &dyn Runner,
    prefix: &Path,
) -> Result<()> {
    let initialized = runner.wineboot(prefix, "--init").await;
    let stopped = shutdown_prefix(runner, prefix).await;
    initialized?;
    stopped
}

pub(crate) async fn shutdown_prefix(runner: &dyn Runner, prefix: &Path) -> Result<()> {
    runner.wineserver(prefix, "-k").await
}

pub(crate) fn detect_runner_kind(path: &Path) -> Result<RunnerKind> {
    if path.join("proton").is_file() {
        Ok(RunnerKind::Proton)
    } else if path.join("bin/wine").is_file() {
        Ok(RunnerKind::Wine)
    } else {
        Err(RunnerError::RunnerNotFound(path.to_path_buf()).into())
    }
}

pub(crate) fn load_runner(
    path: &Path,
    kind: RunnerKind,
    umu_executable: Option<&Path>,
) -> Result<Box<dyn Runner>> {
    if detect_runner_kind(path)? != kind {
        return Err(RunnerError::RunnerNotFound(path.to_path_buf()).into());
    }
    match kind {
        RunnerKind::Wine => Ok(Box::new(Wine::new(path.join("bin/wine"))?)),
        RunnerKind::Proton => Ok(Box::new(Proton::new(
            path,
            umu_executable.ok_or(RunnerError::UmuExecutableMissing)?,
        )?)),
    }
}
