use super::{Proton, Runner, RunnerInfo, Wine};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// UMU (Unified Launcher) runner implementation
///
/// UMU is a universal compatibility layer that wraps other runners like Proton
/// to provide enhanced game compatibility and launcher functionality. It can
/// automatically configure optimal settings for different games and provides
/// a unified interface for various Windows compatibility tools.
#[derive(Debug)]
pub struct UMU {
    info: RunnerInfo,
    /// Underlying Proton runner that UMU wraps
    ///
    /// When present, UMU will use this Proton instance to run applications.
    /// If None, UMU will download the latest Proton version it can find and set that up.
    proton: Option<Proton>,
}

impl UMU {
    pub fn try_from(
        path: &Path,
        proton: Option<Proton>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let executable = PathBuf::from("./umu-run");
        let mut info = RunnerInfo::try_from(path, &executable)?;
        let pretty_version = info
            .version
            .split_whitespace()
            .nth(2)
            .unwrap_or("unknown")
            .to_string();
        info.version = pretty_version;
        Ok(UMU { info, proton })
    }
}

impl Runner for UMU {
    fn wine(&self) -> &Wine {
        // TODO: Make sure an unwrap is possible
        self.proton.as_ref().unwrap().wine()
    }

    fn info(&self) -> &RunnerInfo {
        &self.info
    }

    fn info_mut(&mut self) -> &mut RunnerInfo {
        &mut self.info
    }

    fn initialize(&self, prefix: impl AsRef<Path>) -> Result<(), crate::Error> {
        let proton_path = self.proton.as_ref().unwrap().info().directory();
        Command::new(self.info().executable_path())
            .arg("wineboot") // This is wrong but it'll anyways initialize the prefix
            .env("WINEPREFIX", prefix.as_ref())
            .env("PROTONPATH", proton_path)
            .output()?;
        Ok(())
    }
}
