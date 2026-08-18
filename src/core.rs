#[cfg(feature = "fvs")]
use std::path::PathBuf;
use std::sync::Arc;

use download_manager::manager::{DownloadManager, DownloadManagerConfig};
use http_client::ReqwestClient;
use url::Url;

use crate::{Addons, BottleManager, Context, Directories, error::Result};

#[derive(Clone, Debug, Default)]
pub struct Config {
    #[cfg(feature = "fvs")]
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
            #[cfg(feature = "fvs")]
            fvs2d,
            component_catalog,
            dependency_catalog,
        } = config;
        #[cfg(not(feature = "fvs"))]
        let fvs2d = None;
        // An explicitly configured path always wins; otherwise fall back
        // to resolving `fvs2d` from $PATH, same as a shell would.
        #[cfg(feature = "fvs")]
        let fvs2d = fvs2d.or_else(|| crate::utils::find_in_path("fvs2d"));
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
