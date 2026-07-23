use std::path::{Path, PathBuf};

use ::directories::ProjectDirs;

use uuid::Uuid;

use crate::{bottle::error::BottleError, error::Result};

#[derive(Clone, Debug)]
pub struct Directories {
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Directories {
    pub fn for_project(project_name: &str) -> Result<Self> {
        let project = ProjectDirs::from("com", "usebottles", project_name)
            .ok_or(BottleError::ProjectDirectoriesUnavailable)?;
        let data_dir = project.data_local_dir().to_path_buf();
        let runtime_dir = project
            .runtime_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| data_dir.join("runtime"));
        Ok(Self {
            data_dir,
            runtime_dir,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub fn bottles(&self) -> PathBuf {
        self.data_dir.join("bottles")
    }

    pub fn bottle(&self, id: Uuid) -> PathBuf {
        self.bottles().join(id.to_string())
    }

    pub fn components(&self) -> PathBuf {
        self.data_dir.join("components")
    }

    pub fn dependencies(&self) -> PathBuf {
        self.data_dir.join("dependencies")
    }
}
