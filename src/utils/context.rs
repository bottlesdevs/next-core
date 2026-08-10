use crate::{
    Directories,
    addons::Addons,
    error::{Error, Result},
    utils::absolute_path,
};
use fvs_rs::Fvs2dClient;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::OnceCell;

struct ContextInner {
    directories: Directories,
    fvs2d_executable: Option<PathBuf>,
    fvs: OnceCell<Fvs2dClient>,
    addons: Addons,
}

#[derive(Clone)]
pub(crate) struct Context(Arc<ContextInner>);

impl Context {
    pub(crate) fn new(
        directories: Directories,
        fvs2d_executable: Option<PathBuf>,
        addons: Addons,
    ) -> Result<Self> {
        Ok(Self(Arc::new(ContextInner {
            directories,
            fvs2d_executable: fvs2d_executable.map(absolute_path).transpose()?,
            fvs: OnceCell::new(),
            addons,
        })))
    }

    #[cfg(test)]
    pub(crate) async fn for_test(
        directories: Directories,
        fvs2d_executable: Option<PathBuf>,
    ) -> Result<Self> {
        let client =
            http_client::MockClient::new(|_| Ok(http::Response::new(http_client::body([]))));
        let downloads = download_manager::manager::DownloadManager::new(
            Arc::new(client),
            download_manager::manager::DownloadManagerConfig::default(),
        )?;
        let addons = Addons::load(directories.clone(), None, None, Arc::new(downloads)).await?;
        Self::new(directories, fvs2d_executable, addons)
    }

    pub(crate) fn directories(&self) -> &Directories {
        &self.0.directories
    }

    pub(crate) fn addons(&self) -> &Addons {
        &self.0.addons
    }

    pub(crate) async fn fvs(&self) -> Result<&Fvs2dClient> {
        let executable = self
            .0
            .fvs2d_executable
            .as_ref()
            .ok_or(Error::Fvs2dNotConfigured)?;
        self.0
            .fvs
            .get_or_try_init(|| async {
                Ok(Fvs2dClient::connect_or_spawn(
                    executable,
                    self.0.directories.runtime_dir().join("fvs2d.sock"),
                )
                .await?)
            })
            .await
    }
}
