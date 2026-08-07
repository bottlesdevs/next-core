use std::path::{Path, PathBuf};

use ::directories::ProjectDirs;

use uuid::Uuid;

use crate::{
    addons::catalog::{ItemKind, category},
    bottle::error::BottleError,
    error::Result,
};

#[derive(Clone, Debug)]
pub struct Directories(ProjectDirs);

impl Directories {
    pub(crate) async fn new() -> Result<Self> {
        let directories = Self(
            ProjectDirs::from("com", "usebottles", "bottles-next")
                .ok_or(BottleError::ProjectDirectoriesUnavailable)?,
        );
        for directory in directories.paths() {
            async_fs::create_dir_all(directory).await?;
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

    pub(crate) fn data_dir(&self) -> &Path {
        self.0.data_local_dir()
    }

    pub(crate) fn runtime_dir(&self) -> PathBuf {
        self.0
            .runtime_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.data_dir().join("runtime"))
    }

    pub(crate) fn bottles(&self) -> PathBuf {
        self.data_dir().join("bottles")
    }

    pub(crate) fn bottle(&self, id: Uuid) -> PathBuf {
        self.bottles().join(id.to_string())
    }

    pub(crate) fn components(&self) -> PathBuf {
        self.data_dir().join("components")
    }

    pub(crate) fn component_category(&self, kind: ItemKind) -> Option<PathBuf> {
        category(kind).map(|category| self.components().join(category))
    }

    pub(crate) fn component_index(&self) -> PathBuf {
        self.components().join("index.toml")
    }

    pub(crate) fn dependencies(&self) -> PathBuf {
        self.data_dir().join("dependencies")
    }

    pub(crate) fn dependency(&self, id: Uuid) -> PathBuf {
        self.dependencies().join(id.to_string())
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
