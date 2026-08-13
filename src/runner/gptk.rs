//! Direct GPTK (Game Porting Toolkit) command lowering.
//!
//! GPTK is a Wine-compatible layout that ships `wine64` instead of `wine` and
//! requires its D3DMetal/Rosetta environment on every invocation. Windows
//! commands run through the configured `wine64` executable with `WINEPREFIX`
//! set to the bottle prefix, `WINEARCH=win64`, and the GPTK-specific
//! environment. Server control bypasses GPTK and uses the sibling
//! `wineserver` executable with the same environment.

use super::{Command, Runner, RunnerCommand, RunnerError, Spawnable, Wrapper};
use crate::error::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct Gptk {
    executable: PathBuf,
}

impl Gptk {
    pub fn new(executable: impl AsRef<Path>) -> Self {
        Self {
            executable: executable.as_ref().to_path_buf(),
        }
    }
}

#[async_trait]
impl Runner for Gptk {
    fn command(&self, prefix: &Path, inner: Command) -> RunnerCommand {
        RunnerCommand(
            Command::new(&self.executable)
                .env("WINEPREFIX", prefix)
                .env("WINEARCH", "win64")
                .env("WINEESYNC", "1")
                .env("WINEDEBUG", "-all")
                .env("D3DM_SUPPORT_METAL_LAYER", "1")
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
                .env("WINEARCH", "win64")
                .env("WINEESYNC", "1")
                .env("WINEDEBUG", "-all")
                .env("D3DM_SUPPORT_METAL_LAYER", "1"),
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
