mod proton;
mod wine;

use async_trait::async_trait;
pub use proton::Proton;
use thiserror::Error;
pub use wine::Wine;

use crate::{compatibility::components::catalog::RunnerKind, error::Result};
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::ExitStatus,
};
use tokio::process::Child;

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

pub struct RunnerCommand {
    executable: PathBuf,
    args: Vec<String>,
    envs: HashMap<OsString, OsString>,
}

impl RunnerCommand {
    pub fn new(executable: impl AsRef<Path>) -> Self {
        Self {
            executable: executable.as_ref().to_path_buf(),
            args: Vec::new(),
            envs: HashMap::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs
            .insert(key.as_ref().to_os_string(), value.as_ref().to_os_string());
        self
    }

    pub fn envs(mut self, envs: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        for (key, value) in envs {
            self = self.env(key, value);
        }

        self
    }
}

#[async_trait]
pub trait Runner: Send + Sync {
    fn run(&self, prefix: &Path, command: RunnerCommand) -> Result<Child>;

    async fn wineboot(&self, prefix: &Path, arg: &str) -> Result<()> {
        let status = self
            .run(prefix, RunnerCommand::new("wineboot").arg(arg))?
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
