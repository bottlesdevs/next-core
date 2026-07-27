use crate::{Directories, Operation, error::Result, utils::absolute_path};
use fvs_rs::Fvs2dClient;
use std::{future::Future, path::PathBuf, sync::Arc};
use tokio::{
    runtime::Handle,
    sync::{OnceCell, watch},
};
use tokio_util::sync::CancellationToken;

struct ContextInner {
    directories: Directories,
    fvs2d_executable: PathBuf,
    fvs: OnceCell<Fvs2dClient>,
    runtime: Handle,
}

#[derive(Clone)]
pub struct Context(Arc<ContextInner>);

impl Context {
    pub fn new(directories: Directories, fvs2d_executable: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self(Arc::new(ContextInner {
            directories,
            fvs2d_executable: absolute_path(fvs2d_executable.into())?,
            fvs: OnceCell::new(),
            runtime: Handle::try_current().map_err(std::io::Error::other)?,
        })))
    }

    pub fn directories(&self) -> &Directories {
        &self.0.directories
    }

    pub fn spawn<T, P, F, Fut>(&self, work: F) -> Operation<T, P>
    where
        T: Send + 'static,
        P: Clone + Send + Sync + 'static,
        F: FnOnce(watch::Sender<Option<P>>, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        Operation::spawn(&self.0.runtime, work)
    }

    pub(crate) async fn spawn_blocking<T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        self.0.runtime.spawn_blocking(work).await?
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
