use super::{Command, Runner, RunnerError, Wrapper};
use crate::error::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

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

#[async_trait]
impl Runner for Wine {
    fn command(&self, prefix: &Path, inner: Command) -> Command {
        Command::new(&self.executable)
            .env("WINEPREFIX", prefix)
            .env("WINEARCH", "win64")
            .wrap(inner)
            .into()
    }

    async fn wineserver(&self, prefix: &Path, arg: &str) -> Result<()> {
        let status = Command::new(self.executable.with_file_name("wineserver"))
            .arg(arg)
            .env("WINEPREFIX", prefix)
            .env("WINEARCH", "win64")
            .spawn()?
            .wait()
            .await?;
        if !status.success() {
            return Err(RunnerError::WineserverFailed(status).into());
        }
        Ok(())
    }
}
