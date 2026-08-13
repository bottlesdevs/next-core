//! Direct Wine command lowering.
//!
//! Windows commands run through the configured Wine executable with `WINEPREFIX`
//! set to the bottle prefix and `WINEARCH=win64`. Server control bypasses Wine
//! and uses the sibling `wineserver` executable with the same environment.

use super::{Command, Runner, RunnerCommand, RunnerError, Spawnable, Wrapper};
use crate::error::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct Wine {
    executable: PathBuf,
}

impl Wine {
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

    async fn wine_version(&self) -> Option<super::WineVersion> {
        super::report_version(&self.executable).await
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
