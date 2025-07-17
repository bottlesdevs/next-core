use super::{Runner, RunnerInfo};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Wine {
    info: RunnerInfo,
}

pub enum PrefixArch {
    Win32,
    Win64,
}

pub enum WindowsVersion {
    Win7,
    Win8,
    Win10,
}

impl TryFrom<&Path> for Wine {
    type Error = Box<dyn std::error::Error>;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        let executable = PathBuf::from("./bin/wine");
        let info = RunnerInfo::try_from(path, &executable)?;
        Ok(Wine { info })
    }
}

impl Runner for Wine {
    fn wine(&self) -> &Wine {
        self
    }

    fn info(&self) -> &RunnerInfo {
        &self.info
    }

    fn info_mut(&mut self) -> &mut RunnerInfo {
        &mut self.info
    }
}
