use std::{path::PathBuf, sync::Arc};

use download_manager::manager::{DownloadManager, DownloadManagerConfig};
use http_client::ReqwestClient;
use tokio::runtime::Handle;
use url::Url;

use crate::{BottleManager, Context, Directories, Library, error::Result};

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub fvs2d: Option<PathBuf>,
    pub component_catalog: Option<Url>,
    pub dependency_catalog: Option<Url>,
}

pub struct Bottles {
    bottles: BottleManager,
    library: Library,
    downloader: Arc<DownloadManager>,
}

impl Bottles {
    pub async fn open(config: Config) -> Result<Self> {
        let Config {
            fvs2d,
            component_catalog,
            dependency_catalog,
        } = config;
        let runtime = Handle::current();
        let directories = Directories::new().await?;
        let client = ReqwestClient::new().map_err(download_manager::error::Error::from)?;
        let (downloader, scheduler) =
            DownloadManager::new(Arc::new(client), DownloadManagerConfig::default());
        let _ = runtime.spawn(scheduler);
        let downloader = Arc::new(downloader);
        let library = Library::load(
            directories.clone(),
            component_catalog,
            dependency_catalog,
            downloader.clone(),
        )
        .await?;
        let context = Context::new(directories, fvs2d, library.clone())?;

        Ok(Self {
            bottles: BottleManager::new(context),
            library,
            downloader,
        })
    }

    pub async fn close(self) -> Result<()> {
        self.downloader.shutdown().await;
        Ok(())
    }

    pub fn bottles(&self) -> &BottleManager {
        &self.bottles
    }

    pub fn library(&self) -> &Library {
        &self.library
    }
}
