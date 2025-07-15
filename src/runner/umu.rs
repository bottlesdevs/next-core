use super::{Runner, RunnerInfo};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct UMU {
    info: RunnerInfo,
    proton_path: Option<PathBuf>,
}

impl TryFrom<&Path> for UMU {
    type Error = Box<dyn std::error::Error>;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        let executable = PathBuf::from(Self::EXECUTABLE);
        let mut info = RunnerInfo::try_from(path, &executable)?;
        let pretty_version = info
            .version
            .split_whitespace()
            .nth(2)
            .unwrap_or("unknown")
            .to_string();
        info.version = pretty_version;
        Ok(UMU {
            info,
            proton_path: None,
        })
    }
}

impl Runner for UMU {
    const EXECUTABLE: &'static str = "umu-run";
    fn info(&self) -> &RunnerInfo {
        &self.info
    }
}
