mod proton;
mod umu;
mod wine;

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
}

impl RunnerInfo {
    // This fuction is only meant to be called by the runners themselves hence this is not public
    fn try_from(directory: &Path, executable: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !directory.exists() {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("'{}' does not exist", directory.display()),
            )));
        }
        let full_path = directory.join(executable);

        if !full_path.exists() || !full_path.is_file() {
            return Err(Box::new(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Executable '{}' not found in directory '{}'",
                    executable.display(),
                    directory.display()
                ),
            ))));
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
            .map_err(|e| Error::Io(e))?;

        Ok(RunnerInfo {
            name,
            directory: directory.to_path_buf(),
            version,
        })
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
    const EXECUTABLE: &'static str;
    /// Get the common runner information
    fn info(&self) -> &RunnerInfo;

    /// Check if the runner executable is available and functional
    fn is_available(&self) -> bool {
        let executable_path = self.info().directory().join(Self::EXECUTABLE);
        executable_path.exists() && executable_path.is_file()
    }
}
