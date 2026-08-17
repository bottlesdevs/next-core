use crate::{Directories, error::Result};
use download_manager::manager::DownloadManager;
use std::{path::PathBuf, sync::Arc};
#[cfg(feature = "fvs")]
use {crate::utils::absolute_path, fvs_rs::Fvs2dClient, tokio::sync::OnceCell};

struct ContextInner {
    directories: Directories,
    downloader: Arc<DownloadManager>,
    #[cfg(feature = "fvs")]
    fvs2d_executable: PathBuf,
    #[cfg(feature = "fvs")]
    fvs: OnceCell<Fvs2dClient>,
}

#[derive(Clone)]
pub(crate) struct Context(Arc<ContextInner>);

impl Context {
    pub(crate) fn new(
        directories: Directories,
        downloader: Arc<DownloadManager>,
        fvs2d_executable: Option<PathBuf>,
    ) -> Result<Self> {
        #[cfg(not(feature = "fvs"))]
        let _ = fvs2d_executable;
        Ok(Self(Arc::new(ContextInner {
            directories,
            downloader,
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
    pub(crate) fn for_test(
        directories: Directories,
        fvs2d_executable: Option<PathBuf>,
    ) -> Result<Self> {
        let client =
            http_client::MockClient::new(|_| Ok(http::Response::new(http_client::body([]))));
        let downloader = download_manager::manager::DownloadManager::new(
            Arc::new(client),
            download_manager::manager::DownloadManagerConfig::default(),
        )?;
        Self::new(directories, Arc::new(downloader), fvs2d_executable)
    }

    pub(crate) fn directories(&self) -> &Directories {
        &self.0.directories
    }

    pub(crate) fn downloader(&self) -> &DownloadManager {
        &self.0.downloader
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
        let root = std::env::temp_dir().join(format!("bottles-next-{}", uuid::Uuid::new_v4()));
        let directories = Directories::from_path(&root).unwrap();
        let context = Context::for_test(directories, None).unwrap();

        assert_eq!(context.0.fvs2d_executable, PathBuf::from("fvs2d"));

        drop(context);
        std::fs::remove_dir_all(root).unwrap();
    }
}
