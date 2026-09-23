//! Lowers commands through a direct Wine installation.
//!
//! Guest commands run through the configured executable with `WINEPREFIX` set
//! to the target prefix and `WINEARCH=win64`. Server control uses the sibling
//! `wineserver` executable with the same environment.

use super::{Runner, RunnerCommand, RunnerError};
use crate::command::{Command, Spawnable, Wrapper};
use crate::error::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// Stores the direct Wine executable used for command lowering.
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
        RunnerCommand(command.env("WINEPREFIX", prefix).env("WINEARCH", "win64"))
    }

    async fn wineserver(&self, prefix: &Path, arg: &str) -> Result<()> {
        let status = RunnerCommand(
            Command::new(self.executable.with_file_name("wineserver"))
                .arg(arg)
                .env("WINEPREFIX", prefix)
                .env("WINEARCH", "win64"),
        )
        .spawn()?
        .status()
        .await?;
        if !status.success() {
            return Err(RunnerError::WineserverFailed(status).into());
        }
        Ok(())
    }
}
