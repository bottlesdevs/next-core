#[cfg(target_os = "macos")]
mod gptk;
mod proton;
mod umu;
mod wine;

#[cfg(target_os = "macos")]
pub use gptk::GPTK;
pub use proton::Proton;
pub use umu::UMU;
pub use wine::Wine;

use crate::Error;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// Common information about a runner
#[derive(Debug)]
pub struct RunnerInfo {
    name: String,
    version: String,
    directory: PathBuf,
    executable: PathBuf,
}

impl RunnerInfo {
    // This fuction is only meant to be called by the runners themselves hence this is not public
    fn try_from(directory: &Path, executable: &Path) -> Result<Self, Error> {
        if !directory.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("'{}' does not exist", directory.display()),
            )
            .into());
        }
        let full_path = directory.join(executable);

        if !full_path.exists() || !full_path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Executable '{}' not found in directory '{}'",
                    executable.display(),
                    directory.display()
                ),
            )
            .into());
        }

        let name = directory
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let version = Command::new(&full_path)
            .arg("--version")
            .output()
            .map(|output| {
                let ver = String::from_utf8_lossy(&output.stdout).to_string();
                if ver.is_empty() {
                    name.clone()
                } else {
                    ver
                }
            })
            .map_err(Error::Io)?;

        Ok(RunnerInfo {
            name,
            directory: directory.to_path_buf(),
            executable: executable.to_path_buf(),
            version,
        })
    }

    /// Get the full path to the executable for the runner
    pub fn executable_path(&self) -> PathBuf {
        self.directory.join(&self.executable)
    }
}

impl RunnerInfo {
    /// Name of the runner
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Version of the runner
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Directory where the runner is located
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

pub trait Runner {
    /// Get the Wine runner associated with this runner
    ///
    /// This is possible because all runners are built on top of Wine
    fn wine(&self) -> &Wine;

    /// Get the common runner information
    fn info(&self) -> &RunnerInfo;

    /// Check if the runner executable is available and functional
    fn is_available(&self) -> bool {
        let executable_path = self.info().executable_path();
        executable_path.exists() && executable_path.is_file()
    }
}
