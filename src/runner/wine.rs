//! Lowers commands through a direct Wine installation.
//!
//! Guest commands run through the configured executable with `WINEPREFIX` set
//! to the target prefix. Server control uses the sibling `wineserver`
//! executable with the same environment.

use super::{Runner, RunnerCommand, RunnerError};
use crate::command::{Command, Spawnable, Wrapper};
use crate::error::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct Wine {
    executable: PathBuf,
}

impl Wine {
    /// Creates a runner without validating the executable path.
    pub fn new(executable: impl AsRef<Path>) -> Self {
        Self {
            executable: executable.as_ref().to_path_buf(),
        }
    }
}

#[async_trait]
impl Runner for Wine {
    fn command(&self, prefix: &Path, inner: Command) -> RunnerCommand {
        let command: Command = Command::new(&self.executable).wrap(inner).into();
        RunnerCommand(command.env("WINEPREFIX", prefix))
    }

    async fn wineserver(&self, prefix: &Path, arg: &str) -> Result<()> {
        let status = RunnerCommand(
            Command::new(self.executable.with_file_name("wineserver"))
                .arg(arg)
                .env("WINEPREFIX", prefix),
        )
        .spawn()?
        .status()
        .await?;
        if status.success() || (arg == "-k" && status.code() == Some(1)) {
            return Ok(());
        }
        Err(RunnerError::WineserverFailed(status).into())
    }
}
