//! Shared services used by one core instance.

use crate::{Addons, Directories, error::Result};
use download_manager::manager::{DownloadManager, DownloadManagerConfig};
use http_client::HttpClient;
use std::sync::Arc;
use url::Url;

struct ContextInner {
    directories: Directories,
    http_client: Arc<dyn HttpClient>,
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
        directories: Directories,
        http_client: Arc<dyn HttpClient>,
        component_catalog: Option<Url>,
        dependency_catalog: Option<Url>,
    ) -> Result<Self> {
        let downloader = Arc::new(DownloadManager::new(
            http_client.clone(),
            DownloadManagerConfig::default(),
        )?);
        let addons = Addons::load(
            directories.clone(),
            downloader.clone(),
            component_catalog,
            dependency_catalog,
        )
        .await?;
        Ok(Self(Arc::new(ContextInner {
            directories,
            http_client,
            downloader,
            addons,
            #[cfg(feature = "fvs")]
            fvs,
        })))
    }

    #[cfg(test)]
    /// Constructs a context backed by a successful empty HTTP mock.
    ///
    /// # Errors
    ///
    /// Returns the initialization and catalog-loading errors from [`Self::new`].
    ///
    /// # Panics
    ///
    /// With the `fvs` feature, panics if the process cannot construct the
    /// test-only Tokio runtime used to create a lazy FVS channel.
    pub(crate) async fn for_test(directories: Directories) -> Result<Self> {
        let client = Arc::new(http_client::MockClient::new(|_| {
            Ok(http::Response::new(http_client::body([])))
        }));
        Self::new(
            #[cfg(feature = "fvs")]
            crate::virgo::Fvs::for_test(&directories),
            directories,
            client,
            None,
            None,
        )
        .await
    }

    pub(crate) fn addons(&self) -> &Addons {
        &self.0.addons
    }

    pub(crate) fn directories(&self) -> &Directories {
        &self.0.directories
    }

    pub(crate) fn downloader(&self) -> &DownloadManager {
        &self.0.downloader
    }

    pub(crate) fn http_client(&self) -> &Arc<dyn HttpClient> {
        &self.0.http_client
    }

    #[cfg(feature = "fvs")]
    pub(crate) fn fvs(&self) -> &crate::virgo::Fvs {
        &self.0.fvs
    }
}
