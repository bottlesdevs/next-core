use super::{Command, Runner, RunnerCommand, RunnerError, Spawnable, Wrapper};
use crate::error::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// Wine runner implementation
///
/// Wine is the base compatibility layer that all other runners build upon. It provides
/// the core Windows API translation functionality that allows Windows applications
/// to run on Unix-like systems.
#[derive(Debug)]
pub(crate) struct Wine {
    executable: PathBuf,
}

impl Wine {
    /// Creates a new Wine runner with the specified executable path
    pub fn new(executable: impl AsRef<Path>) -> Self {
        Self {
            executable: executable.as_ref().to_path_buf(),
        }
    }
}

#[async_trait]
impl Runner for Wine {
    fn command(&self, prefix: &Path, inner: Command) -> RunnerCommand {
        RunnerCommand(
            Command::new(&self.executable)
                .env("WINEPREFIX", prefix)
                .env("WINEARCH", "win64")
                .wrap(inner)
                .into(),
        )
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
