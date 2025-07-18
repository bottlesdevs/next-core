use super::{Runner, RunnerInfo};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Wine runner implementation
///
/// Wine is the base compatibility layer that all other runners build upon. It provides
/// the core Windows API translation functionality that allows Windows applications
/// to run on Unix-like systems.
#[derive(Debug)]
pub struct Wine {
    info: RunnerInfo,
}

/// Architecture for Wine prefix creation
///
/// Determines whether a Wine prefix should be configured for 32-bit or 64-bit
/// Windows compatibility. This affects which Windows applications can run
/// in the prefix
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixArch {
    /// 32-bit Windows prefix architecture
    Win32,
    /// 64-bit Windows prefix architecture (recommended)
    Win64,
}

/// Windows version compatibility settings
///
/// Specifies which version of Windows the Wine prefix should emulate.
/// Different applications may require specific Windows versions for
/// optimal compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    fn initialize(&self, prefix: impl AsRef<Path>) -> Result<(), crate::Error> {
        // FIXME: Launch winebridge to initialize the prefix
        Command::new(self.info().executable_path())
            .arg("wineboot")
            .arg("--init")
            .env("WINEPREFIX", prefix.as_ref())
            .output()?;

        Ok(())
    }
}
