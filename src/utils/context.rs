use crate::{Directories, error::Result, utils::absolute_path};
use fvs_rs::Fvs2dClient;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::OnceCell;

struct ContextInner {
    directories: Directories,
    fvs2d_executable: PathBuf,
    fvs: OnceCell<Fvs2dClient>,
}

#[derive(Clone)]
pub(crate) struct Context(Arc<ContextInner>);

impl Context {
    pub(crate) fn new(
        directories: Directories,
        fvs2d_executable: impl Into<PathBuf>,
    ) -> Result<Self> {
        Ok(Self(Arc::new(ContextInner {
            directories,
            fvs2d_executable: absolute_path(fvs2d_executable.into())?,
            fvs: OnceCell::new(),
        })))
    }

    pub(crate) fn directories(&self) -> &Directories {
        &self.0.directories
    }

    pub(crate) async fn spawn_blocking<T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        tokio::task::spawn_blocking(work).await?
    }

    pub(crate) async fn fvs(&self) -> Result<&Fvs2dClient> {
        self.0
            .fvs
            .get_or_try_init(|| async {
                Ok(Fvs2dClient::connect_or_spawn(
                    &self.0.fvs2d_executable,
                    self.0.directories.runtime_dir().join("fvs2d.sock"),
                )
                .await?)
            })
            .await
    }
}
