use super::{Proton, Runner, RunnerInfo, Wine};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct UMU {
    info: RunnerInfo,
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
}
