//! Catalog refresh and cache replacement.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    Operation, Progress, Stage,
    error::{Error, Result},
    utils::storage,
};

use super::super::{
    CatalogError, Component, Dependency,
    catalog::{AddonFamily, Catalog},
};
use super::{Addons, download};

impl Addons {
    /// Refreshes the two configured catalogs independently.
    ///
    /// Each successful catalog is validated, cached, and published even if the
    /// other family fails. A failed family keeps its previously loaded catalog.
    /// If either family fails, the operation returns [`CatalogError::Refresh`]
    /// after publishing every successful result.
    ///
    /// # Errors
    ///
    /// The operation fails when a URL is not configured, a download or catalog
    /// validation fails, a successful catalog cannot be cached, refreshed state
    /// cannot be loaded, or cancellation is requested before publication.
    pub fn refresh(&self) -> Operation<()> {
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let component = addons
                .download_catalog::<Component>(progress.clone(), &cancellation)
                .await;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let dependency = addons
                .download_catalog::<Dependency>(progress, &cancellation)
                .await;

            let _write = cancellation
                .run_until_cancelled(addons.0.write.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let current = addons.state();
            let component_catalog = match &component {
                Ok(catalog) => {
                    catalog.save(&addons.0.directories).await?;
                    Some(catalog.clone())
                }
                Err(_) => current.component_catalog.clone(),
            };
            let dependency_catalog = match &dependency {
                Ok(catalog) => {
                    catalog.save(&addons.0.directories).await?;
                    Some(catalog.clone())
                }
                Err(_) => current.dependency_catalog.clone(),
            };
            let mut next = current.as_ref().clone();
            next.component_catalog = component_catalog;
            next.dependency_catalog = dependency_catalog;
            addons.publish(next);

            match (component, dependency) {
                (Ok(_), Ok(_)) => Ok(()),
                (component, dependency) => Err(CatalogError::Refresh {
                    components: component.err().map(|error| error.to_string()),
                    dependencies: dependency.err().map(|error| error.to_string()),
                }
                .into()),
            }
        })
    }

    /// Downloads and validates one catalog through a best-effort temporary file.
    async fn download_catalog<K>(
        &self,
        progress: watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<Arc<Catalog<K>>>
    where
        K: AddonFamily,
        Catalog<K>: DeserializeOwned,
    {
        let url = K::url(&self.0.catalog_urls).ok_or(CatalogError::UrlNotConfigured(K::LABEL))?;
        storage::with_temp_dir(&self.0.directories.staging(), |stage| async move {
            let downloaded = stage.join("catalog.json");
            download(
                &self.0.downloader,
                url,
                &downloaded,
                cancellation,
                |transfer| {
                    progress.send_replace(Some(Progress::transferring(
                        Stage::Downloading {
                            file: format!("{} catalog", K::LABEL),
                        },
                        transfer,
                    )));
                },
            )
            .await?;
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            Ok(Arc::new(serde_json::from_slice::<Catalog<K>>(
                &async_fs::read(&downloaded).await?,
            )?))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Context, Directories};
    use uuid::Uuid;

    #[test]
    fn cancelled_refresh_does_not_wait_for_publication_lock() {
        futures_lite::future::block_on(async {
            let root = std::env::temp_dir().join(format!("bottles-next-{}", Uuid::new_v4()));
            let directories = Directories::from_path(&root).unwrap();
            let context = Context::for_test(directories).await.unwrap();
            let addons = context.addons().clone();
            let write = addons.0.write.lock().await;
            let mut refresh = addons.refresh();

            assert!(
                futures_lite::future::poll_once(&mut refresh)
                    .await
                    .is_none()
            );
            refresh.cancellation_token().cancel();
            assert!(matches!(refresh.await, Err(Error::Cancelled)));

            drop(write);
            std::fs::remove_dir_all(root).unwrap();
        });
    }
}
