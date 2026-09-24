//! Resolves and names the filesystem locations owned by Bottles core.

use std::path::{Path, PathBuf};

use ::directories::ProjectDirs;

use crate::error::{Error, Result};

/// Resolves configuration, local data, cache, and runtime locations for Bottles.
///
/// Paths follow the host platform's conventions. Constructing this type with
/// [`new`](Self::new) creates the persistent directories required by core
/// services. Cache, staging, trash, program, and Virgo directories are created
/// lazily by the workflows that use them.
///
/// # Examples
///
/// ```no_run
/// use bottles_core::Directories;
///
/// # async fn example() -> bottles_core::error::Result<()> {
/// let directories = Directories::new().await?;
/// let plugin_root = directories.plugins();
/// assert!(plugin_root.starts_with(directories.data_dir()));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Directories(ProjectDirs);

impl Directories {
    /// Resolves platform paths and creates the directories needed at startup.
    ///
    /// Configuration, local data, runtime, bottle, component, dependency, plugin,
    /// and addon-release metadata directories are created recursively.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ProjectDirectoriesUnavailable`] if the platform cannot
    /// determine application directories, or an I/O error if a required
    /// directory cannot be created.
    ///
    /// [`Error::ProjectDirectoriesUnavailable`]: crate::error::Error::ProjectDirectoriesUnavailable
    pub async fn new() -> Result<Self> {
        let directories = Self(
            ProjectDirs::from("com", "usebottles", "bottles-next")
                .ok_or(Error::ProjectDirectoriesUnavailable)?,
        );
        for directory in directories.paths() {
            async_fs::create_dir_all(directory).await?;
        }
        Ok(directories)
    }

    #[cfg(test)]
    /// Creates a test directory layout rooted at `path`.
    pub(crate) fn from_path(path: impl Into<PathBuf>) -> Result<Self> {
        let directories =
            Self(ProjectDirs::from_path(path.into()).ok_or(Error::ProjectDirectoriesUnavailable)?);
        for directory in directories.paths() {
            std::fs::create_dir_all(directory)?;
        }
        Ok(directories)
    }

    pub(crate) fn config_dir(&self) -> &Path {
        self.0.config_dir()
    }

    /// Returns the persistent, platform-local data directory for Bottles.
    pub fn data_dir(&self) -> &Path {
        self.0.data_local_dir()
    }

    /// Returns the disposable platform cache directory for Bottles.
    ///
    /// [`new`](Self::new) does not eagerly create this directory.
    pub fn cache_dir(&self) -> &Path {
        self.0.cache_dir()
    }

    /// Returns the root used for disposable installation and publication workspaces.
    ///
    /// The path is placed under [`Self::data_dir`], allowing completed trees to
    /// be atomically renamed into managed data directories on the same filesystem.
    /// The directory itself is created lazily.
    pub fn staging(&self) -> PathBuf {
        self.data_dir().join(".staging")
    }

    pub(crate) fn trash(&self) -> PathBuf {
        self.data_dir().join(".trash")
    }

    /// Returns the runtime directory used for sockets and ephemeral state.
    ///
    /// Platforms without a dedicated runtime directory fall back to a
    /// `runtime` directory below [`data_dir`](Self::data_dir).
    pub(crate) fn runtime_dir(&self) -> PathBuf {
        self.0
            .runtime_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.data_dir().join("runtime"))
    }

    pub(crate) fn bottles(&self) -> PathBuf {
        self.data_dir().join("bottles")
    }

    #[cfg(feature = "fvs")]
    pub(crate) fn programs(&self) -> PathBuf {
        self.data_dir().join("programs")
    }
    pub(crate) fn components(&self) -> PathBuf {
        self.data_dir().join("components")
    }

    pub(crate) fn dependencies(&self) -> PathBuf {
        self.data_dir().join("dependencies")
    }

    pub(crate) fn component_releases(&self) -> PathBuf {
        self.components().join("releases")
    }

    pub(crate) fn dependency_releases(&self) -> PathBuf {
        self.dependencies().join("releases")
    }

    pub(crate) fn component_catalog(&self) -> PathBuf {
        self.components().join("catalog.json")
    }

    pub(crate) fn dependency_catalog(&self) -> PathBuf {
        self.dependencies().join("catalog.json")
    }

    #[cfg(feature = "fvs")]
    pub(crate) fn virgo(&self) -> PathBuf {
        self.data_dir().join("virgo")
    }

    /// Returns the persistent directory containing installed plugins.
    pub fn plugins(&self) -> PathBuf {
        self.data_dir().join("plugins")
    }

    pub(crate) fn profiles(&self) -> PathBuf {
        self.config_dir().join("profiles.toml")
    }

    fn paths(&self) -> [PathBuf; 9] {
        [
            self.config_dir().to_path_buf(),
            self.data_dir().to_path_buf(),
            self.runtime_dir(),
            self.bottles(),
            self.components(),
            self.dependencies(),
            self.plugins(),
            self.component_releases(),
            self.dependency_releases(),
        ]
    }
}
