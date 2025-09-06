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

/// Contains metadata and paths for any runner implementation. This struct is used
/// internally by all runner types to store their basic information.
#[derive(Debug)]
pub struct RunnerInfo {
    /// Human-readable name of the runner, typically derived from the directory name
    name: String,
    /// Version string obtained from the runner's `--version` output
    version: String,
    /// Base directory where the runner is installed
    directory: PathBuf,
    /// Relative path to the main executable within the directory
    executable: PathBuf,
}

impl RunnerInfo {
    /// Create a new RunnerInfo by validating the directory and executable
    ///
    /// This function is only meant to be called by the runners themselves, hence it's not public.
    /// It performs validation to ensure the directory exists and contains the specified executable.
    ///
    /// # Arguments
    ///
    /// * `directory` - The base directory where the runner is installed
    /// * `executable` - The relative path to the executable within the directory
    ///
    /// # Returns
    ///
    /// Returns a `RunnerInfo` instance if validation succeeds
    ///
    /// # Errors
    ///
    /// This function will return an error if the directory or executable path is invalid,
    /// or if the executable cannot be executed to determine its version.
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
                if ver.is_empty() { name.clone() } else { ver }
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
    ///
    /// Combines the base directory with the relative executable path to provide
    /// the complete path that can be used to execute the runner.
    ///
    /// # Returns
    ///
    /// A `PathBuf` containing the full path to the runner's executable
    pub fn executable_path(&self) -> PathBuf {
        self.directory.join(&self.executable)
    }
}

impl RunnerInfo {
    /// Get the name of the runner
    ///
    /// Returns the human-readable name, typically derived from the directory name
    /// where the runner is installed.
    ///
    /// # Returns
    ///
    /// A string slice containing the runner's name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the version of the runner
    ///
    /// Returns the version string as reported by the runner's `--version` command.
    /// If the version cannot be determined, this may return the runner's name instead.
    ///
    /// # Returns
    ///
    /// A string slice containing the runner's version information
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the directory path where the runner is installed.
    ///
    /// # Returns
    ///
    /// A path reference to the runner's installation directory
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

/// Trait defining the common interface for all Windows compatibility runners
///
/// All runners in this module implement this trait, providing a unified way to interact
/// with different compatibility layers like Wine, Proton, UMU, and GPTK.
pub trait Runner {
    /// Get the Wine runner associated with this runner
    ///
    /// All runners are built on top of Wine, so this method provides access to the
    /// underlying Wine instance. This allows for Wine-specific operations and
    /// configuration even when using higher-level runners like Proton.
    ///
    /// # Returns
    ///
    /// A reference to the underlying Wine runner instance
    fn wine(&self) -> &Wine;

    /// Provides access to metadata about the runner including its name, version,
    /// installation directory, and executable path.
    ///
    /// # Returns
    ///
    /// A reference to the runner's information structure
    fn info(&self) -> &RunnerInfo;

    /// Get a mutable reference to the common runner information
    ///
    /// Allows modification of the runner's metadata. This is typically used
    /// internally by runner implementations during initialization.
    ///
    /// # Returns
    ///
    /// A mutable reference to the runner's information structure
    fn info_mut(&mut self) -> &mut RunnerInfo;

    /// Performs basic validation to ensure the runner can be executed. The default
    /// implementation checks if the executable file exists and is accessible.
    /// Individual runners may override this to perform additional checks.
    ///
    /// # Returns
    ///
    /// `true` if the runner appears to be functional, `false` otherwise
    ///
    /// # Example
    ///
    /// ```rust
    /// use bottles_core::runner::{Wine, Runner};
    /// use std::path::Path;
    ///
    /// let wine_path = Path::new("/usr/bin/wine");
    /// if let Ok(wine) = Wine::try_from(wine_path) {
    ///     if wine.is_available() {
    ///         tracing::info!("Wine is ready to use");
    ///     } else {
    ///         tracing::info!("Wine is not available");
    ///     }
    /// }
    /// ```
    fn is_available(&self) -> bool {
        let executable_path = self.info().executable_path();
        executable_path.exists() && executable_path.is_file()
    }

    /// Initialize a prefix at the specified path using the runner's executable.
    ///
    /// # Arguments
    ///
    /// * `prefix` - Path where the new prefix should be created. The directory will be
    ///   created if it doesn't exist.
    fn initialize(&self, prefix: impl AsRef<Path>) -> Result<(), Error>;
}
