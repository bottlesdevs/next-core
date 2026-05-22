use std::{
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    process::{Child, Command},
};

use crate::{error::Result, runner::RunnerError};

use super::{PrefixConfig, Runner, RunnerCommand};

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
        if !executable.as_ref().exists() {
            return Err(io::Error::new(ErrorKind::NotFound, "Wine executable not found").into());
        }

        if !executable.as_ref().is_file() {
            return Err(
                io::Error::new(ErrorKind::InvalidInput, "Wine executable is not a file").into(),
            );
        }

        Ok(Self {
            executable: executable.as_ref().to_path_buf(),
        })
    }
}

impl Runner for Wine {
    fn run(&self, prefix: &PrefixConfig, command: RunnerCommand) -> Result<Child> {
        Command::new(&self.executable)
            .arg(command.executable)
            .args(command.args)
            .envs(prefix.to_env())
            .envs(command.envs)
            .spawn()
            .map_err(Into::into)
    }

    fn initialize_prefix(&self, prefix: &PrefixConfig) -> Result<()> {
        let command = RunnerCommand::builder()
            .executable("wineboot")
            .arg("--init")
            .build()
            .map_err(Into::<RunnerError>::into)?;

        let status = self.run(prefix, command)?.wait()?;
        if !status.success() {
            return Err(RunnerError::PrefixInitFailed.into());
        }

        Ok(())
    }
}
