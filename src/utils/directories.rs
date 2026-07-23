use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ::directories::ProjectDirs;
use fvs_rs::Fvs2dClient;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::{bottle::error::BottleError, error::Result, utils::absolute_path};

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

struct ContextInner {
    directories: Directories,
    fvs2d_executable: PathBuf,
    fvs: OnceCell<Fvs2dClient>,
}

#[derive(Clone)]
pub struct Context(Arc<ContextInner>);

impl Context {
    pub fn new(directories: Directories, fvs2d_executable: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self(Arc::new(ContextInner {
            directories,
            fvs2d_executable: absolute_path(fvs2d_executable.into())?,
            fvs: OnceCell::new(),
        })))
    }

    pub fn directories(&self) -> &Directories {
        &self.0.directories
    }

    pub(crate) async fn fvs(&self) -> Result<&Fvs2dClient> {
        self.0
            .fvs
            .get_or_try_init(|| async {
                std::fs::create_dir_all(self.0.directories.runtime_dir())?;
                Ok(Fvs2dClient::connect_or_spawn(
                    &self.0.fvs2d_executable,
                    self.0.directories.runtime_dir().join("fvs2d.sock"),
                )
                .await?)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts_keep_independent_roots_and_fvs_configuration() {
        let left = Directories {
            data_dir: "/left/data".into(),
            runtime_dir: "/left/run".into(),
        };
        let right = Directories {
            data_dir: "/right/data".into(),
            runtime_dir: "/right/run".into(),
        };
        let left = Context::new(left, "/left/fvs2d").unwrap();
        let right = Context::new(right, "/right/fvs2d").unwrap();

        assert_eq!(left.directories().data_dir(), Path::new("/left/data"));
        assert_eq!(right.directories().data_dir(), Path::new("/right/data"));
        assert_eq!(left.0.fvs2d_executable, Path::new("/left/fvs2d"));
        assert_eq!(right.0.fvs2d_executable, Path::new("/right/fvs2d"));
        assert!(!Arc::ptr_eq(&left.0, &right.0));
    }
}
