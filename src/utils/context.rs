//! Shared services and addon ownership for one core instance.

use crate::{Addons, Directories, error::Result};
use download_manager::manager::{DownloadManager, DownloadManagerConfig};
use http_client::HttpClient;
use std::{path::PathBuf, sync::Arc};
use url::Url;
#[cfg(feature = "fvs")]
use {crate::utils::absolute_path, fvs_rs::Fvs2dClient, tokio::sync::OnceCell};

struct ContextInner {
    directories: Directories,
    http_client: Arc<dyn HttpClient>,
    downloader: Arc<DownloadManager>,
    addons: Addons,
    #[cfg(feature = "fvs")]
    fvs2d_executable: PathBuf,
    #[cfg(feature = "fvs")]
    fvs: OnceCell<Fvs2dClient>,
}

#[derive(Clone)]
pub(crate) struct Context(Arc<ContextInner>);

impl Context {
    pub(crate) async fn new(
        directories: Directories,
        http_client: Arc<dyn HttpClient>,
        fvs2d_executable: Option<PathBuf>,
        component_catalog: Option<Url>,
        dependency_catalog: Option<Url>,
    ) -> Result<Self> {
        #[cfg(not(feature = "fvs"))]
        let _ = fvs2d_executable;
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
            fvs2d_executable: fvs2d_executable
                .map(absolute_path)
                .transpose()?
                .unwrap_or_else(|| PathBuf::from("fvs2d")),
            #[cfg(feature = "fvs")]
            fvs: OnceCell::new(),
        })))
    }

    #[cfg(test)]
    pub(crate) async fn for_test(
        directories: Directories,
        fvs2d_executable: Option<PathBuf>,
    ) -> Result<Self> {
        let client = Arc::new(http_client::MockClient::new(|_| {
            Ok(http::Response::new(http_client::body([])))
        }));
        Self::new(directories, client, fvs2d_executable, None, None).await
    }

    /// The addon manager shared by discovery and execution workflows.
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

#[cfg(all(test, feature = "fvs"))]
mod tests {
    use super::*;

    #[test]
    fn uses_path_lookup_when_fvs2d_is_not_configured() {
        futures_lite::future::block_on(async {
            let root = std::env::temp_dir().join(format!("bottles-next-{}", uuid::Uuid::new_v4()));
            let directories = Directories::from_path(&root).unwrap();
            let context = Context::for_test(directories, None).await.unwrap();

            assert_eq!(context.0.fvs2d_executable, PathBuf::from("fvs2d"));

            drop(context);
            std::fs::remove_dir_all(root).unwrap();
        });
    }
}
