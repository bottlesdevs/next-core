use super::{Runner, RunnerCommand};
use crate::{error::Result, runner::RunnerError};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
};

/// Wine runner implementation
///
/// Wine is the base compatibility layer that all other runners build upon. It provides
/// the core Windows API translation functionality that allows Windows applications
/// to run on Unix-like systems.
#[derive(Debug)]
pub struct Wine {
    executable: PathBuf,
}

impl Wine {
    /// Creates a new Wine runner with the specified executable path
    pub fn new(executable: impl AsRef<Path>) -> Result<Self> {
        if !executable.as_ref().is_file() {
            return Err(
                RunnerError::RunnerExecutableNotFound(executable.as_ref().to_path_buf()).into(),
            );
        }

        Ok(Self {
            executable: executable.as_ref().to_path_buf(),
        })
    }
}

impl Runner for Wine {
    fn run(&self, prefix: &Path, command: RunnerCommand) -> Result<Child> {
        Command::new(&self.executable)
            .arg(command.executable)
            .args(command.args)
            .env("WINEPREFIX", prefix)
            .env("WINEARCH", "win64")
            .envs(command.envs)
            .spawn()
            .map_err(Into::into)
    }

    fn wineserver(&self, prefix: &Path, arg: &str) -> Result<()> {
        let status = Command::new(self.executable.with_file_name("wineserver"))
            .arg(arg)
            .env("WINEPREFIX", prefix)
            .env("WINEARCH", "win64")
            .status()?;
        if !status.success() {
            return Err(RunnerError::WineserverFailed(status).into());
        }
        Ok(())
    }
}
