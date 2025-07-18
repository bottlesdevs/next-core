use super::{Runner, RunnerInfo, Wine};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// Proton runner implementation
///
/// Proton is Valve's Wine fork designed specifically for gaming on Linux. It includes
/// numerous patches and enhancements over standard Wine, making it particularly
/// effective for running Windows games through Steam or standalone.
///
/// # Note
/// When used outside of Steam, Proton requires specific environment variables:
/// - `STEAM_COMPAT_DATA_PATH`: Path to store compatibility data
/// - `STEAM_COMPAT_CLIENT_INSTALL_PATH`: Steam installation directory
#[derive(Debug)]
pub struct Proton {
    info: RunnerInfo,
    wine: Wine,
}

impl TryFrom<&Path> for Proton {
    type Error = Box<dyn std::error::Error>;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        let executable = PathBuf::from("./proton");
        let info = RunnerInfo::try_from(path, &executable)?;
        let mut wine = Wine::try_from(path.join("files").as_path())?;
        wine.info_mut().name = info.name.clone();
        Ok(Proton { wine, info })
    }
}

impl Runner for Proton {
    fn wine(&self) -> &Wine {
        &self.wine
    }

    fn info(&self) -> &RunnerInfo {
        &self.info
    }

    fn info_mut(&mut self) -> &mut RunnerInfo {
        &mut self.info
    }

    fn initialize(&self, prefix: impl AsRef<Path>) -> Result<(), crate::Error> {
        // FIXME: Launch winebridge to initialize the prefix
        Command::new(self.info().executable_path())
            .arg("run")
            .arg("wineboot")
            .env("WINEPREFIX", prefix.as_ref())
            .env("STEAM_COMPAT_DATA_PATH", prefix.as_ref())
            .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", "")
            .output()?;

        Ok(())
    }
}
