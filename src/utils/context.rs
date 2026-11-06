//! Shared services used by one core instance.

use crate::{Addons, error::Result};
use download_manager::manager::{DownloadManager, DownloadManagerConfig};
use http_client::HttpClient;
use std::sync::Arc;
use url::Url;

struct ContextInner {
    downloader: Arc<DownloadManager>,
    addons: Addons,
    #[cfg(feature = "fvs")]
    fvs: crate::virgo::Fvs,
}

#[derive(Clone)]
pub(crate) struct Context(Arc<ContextInner>);

impl Context {
    /// Constructs shared services and loads persisted addon catalogs.
    ///
    /// # Errors
    ///
    /// Returns an error if the download manager cannot initialize or either
    /// addon catalog cannot be loaded from storage.
    pub(crate) async fn new(
        #[cfg(feature = "fvs")] fvs: crate::virgo::Fvs,
        http_client: Arc<dyn HttpClient>,
        component_catalog: Option<Url>,
        dependency_catalog: Option<Url>,
    ) -> Result<Self> {
        let downloader = Arc::new(DownloadManager::new(
            http_client,
            DownloadManagerConfig::default(),
        )?);
        let addons =
            Addons::load(downloader.clone(), component_catalog, dependency_catalog).await?;
        Ok(Self(Arc::new(ContextInner {
            downloader,
            addons,
            #[cfg(feature = "fvs")]
            fvs,
        })))
    }

    pub(crate) fn addons(&self) -> &Addons {
        &self.0.addons
    }

    pub(crate) fn downloader(&self) -> &DownloadManager {
        &self.0.downloader
    }

    #[cfg(feature = "fvs")]
    pub(crate) fn fvs(&self) -> &crate::virgo::Fvs {
        &self.0.fvs
    }
}
