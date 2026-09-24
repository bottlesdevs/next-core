//! Shared services used by one core instance.

use crate::{Addons, Directories, error::Result};
use download_manager::manager::{DownloadManager, DownloadManagerConfig};
#[cfg(feature = "fvs")]
use fvs_rs::Fvs2dClient;
use http_client::HttpClient;
use std::sync::Arc;
use url::Url;

struct ContextInner {
    directories: Directories,
    http_client: Arc<dyn HttpClient>,
    downloader: Arc<DownloadManager>,
    addons: Addons,
    #[cfg(feature = "fvs")]
    fvs: Arc<Fvs2dClient>,
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
        directories: Directories,
        http_client: Arc<dyn HttpClient>,
        #[cfg(feature = "fvs")] fvs: Arc<Fvs2dClient>,
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
    /// Panics if the process cannot construct the test-only Tokio runtime used
    /// to create a lazy FVS channel.
    pub(crate) async fn for_test(directories: Directories) -> Result<Self> {
        let client = Arc::new(http_client::MockClient::new(|_| {
            Ok(http::Response::new(http_client::body([])))
        }));
        #[cfg(feature = "fvs")]
        let fvs = {
            // Metadata and cancellation tests never issue FVS RPCs.
            static RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> =
                std::sync::LazyLock::new(|| {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                });
            let _entered = RUNTIME.enter();
            Arc::new(Fvs2dClient::from_channel(
                tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy(),
            ))
        };
        Self::new(
            directories,
            client,
            #[cfg(feature = "fvs")]
            fvs,
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
    pub(crate) fn fvs(&self) -> &Arc<Fvs2dClient> {
        &self.0.fvs
    }
}
