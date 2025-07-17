use super::{Runner, RunnerInfo, Wine};
use std::path::{Path, PathBuf};

// TODO: These need to be set to use proton outside steam
// STEAM_COMPAT_DATA_PATH
// STEAM_COMPAT_CLIENT_INSTALL_PATH
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
}
