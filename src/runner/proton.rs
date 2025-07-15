use super::{Runner, RunnerInfo};
use std::path::{Path, PathBuf};

// TODO: These need to be set to use proton outside steam
// STEAM_COMPAT_DATA_PATH
// STEAM_COMPAT_CLIENT_INSTALL_PATH
#[derive(Debug)]
pub struct Proton {
    info: RunnerInfo,
}

impl TryFrom<&Path> for Proton {
    type Error = Box<dyn std::error::Error>;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        let executable = PathBuf::from(Self::EXECUTABLE);
        let info = RunnerInfo::try_from(path, &executable)?;
        Ok(Proton { info })
    }
}

impl Runner for Proton {
    const EXECUTABLE: &'static str = "proton";
    fn info(&self) -> &RunnerInfo {
        &self.info
    }
}
