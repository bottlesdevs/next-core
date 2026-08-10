use std::{path::PathBuf, sync::Arc};

use download_manager::manager::{DownloadManager, DownloadManagerConfig};
use http_client::ReqwestClient;
use url::Url;

use crate::{Addons, BottleManager, Context, Directories, error::Result};

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub fvs2d: Option<PathBuf>,
    pub component_catalog: Option<Url>,
    pub dependency_catalog: Option<Url>,
}

pub struct Bottles {
    bottles: BottleManager,
    addons: Addons,
    downloader: Arc<DownloadManager>,
}

impl Bottles {
    pub async fn open(config: Config) -> Result<Self> {
        let Config {
            fvs2d,
            component_catalog,
            dependency_catalog,
        } = config;
        let directories = Directories::new().await?;
        let client = ReqwestClient::new().map_err(download_manager::error::Error::from)?;
        let downloader = Arc::new(DownloadManager::new(
            Arc::new(client),
            DownloadManagerConfig::default(),
        )?);
        let addons = Addons::load(
            directories.clone(),
            component_catalog,
            dependency_catalog,
            downloader.clone(),
        )
        .await?;
        let context = Context::new(directories, fvs2d, addons.clone())?;
        let bottles = BottleManager::load(context).await?;

        Ok(Self {
            bottles,
            addons,
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

    pub fn addons(&self) -> &Addons {
        &self.addons
    }
}
