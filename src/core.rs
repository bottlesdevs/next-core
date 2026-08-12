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
    context: Context,
    bottles: BottleManager,
    addons: Addons,
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
        let context = Context::new(directories, downloader.clone(), fvs2d)?;
        let addons = Addons::load(context.clone(), component_catalog, dependency_catalog).await?;
        let bottles = BottleManager::load(context.clone(), addons.clone()).await?;

        Ok(Self {
            context,
            bottles,
            addons,
        })
    }

    pub async fn close(self) -> Result<()> {
        self.context.downloader().shutdown().await;
        Ok(())
    }

    pub fn bottles(&self) -> &BottleManager {
        &self.bottles
    }

    pub fn addons(&self) -> &Addons {
        &self.addons
    }
}
