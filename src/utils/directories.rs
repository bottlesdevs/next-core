use std::path::{Path, PathBuf};

use ::directories::ProjectDirs;

use uuid::Uuid;

use crate::{bottle::error::BottleError, error::Result};

#[derive(Clone, Debug)]
pub struct Directories(ProjectDirs);

impl Directories {
    pub async fn for_project(project_name: &str) -> Result<Self> {
        let directories = Self(
            ProjectDirs::from("com", "usebottles", project_name)
                .ok_or(BottleError::ProjectDirectoriesUnavailable)?,
        );
        for directory in directories.paths() {
            tokio::fs::create_dir_all(directory).await?;
        }
        Ok(directories)
    }

    #[cfg(test)]
    pub(crate) fn from_path(path: impl Into<PathBuf>) -> Result<Self> {
        let directories = Self(
            ProjectDirs::from_path(path.into())
                .ok_or(BottleError::ProjectDirectoriesUnavailable)?,
        );
        for directory in directories.paths() {
            std::fs::create_dir_all(directory)?;
        }
        Ok(directories)
    }

    pub fn data_dir(&self) -> &Path {
        self.0.data_local_dir()
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.0
            .runtime_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.data_dir().join("runtime"))
    }

    pub fn bottles(&self) -> PathBuf {
        self.data_dir().join("bottles")
    }

    pub fn bottle(&self, id: Uuid) -> PathBuf {
        self.bottles().join(id.to_string())
    }

    pub fn components(&self) -> PathBuf {
        self.data_dir().join("components")
    }

    pub fn dependencies(&self) -> PathBuf {
        self.data_dir().join("dependencies")
    }

    fn paths(&self) -> [PathBuf; 5] {
        [
            self.data_dir().to_path_buf(),
            self.runtime_dir(),
            self.bottles(),
            self.components(),
            self.dependencies(),
        ]
    }
}
