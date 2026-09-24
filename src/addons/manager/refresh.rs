//! Remote catalog refresh and publication.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    Operation, Progress, Stage,
    error::{Error, Result},
    utils::fs,
};

use super::super::{
    CatalogError, Component, Dependency,
    catalog::{AddonFamily, Catalog},
};
use super::{Addons, download::download};

impl Addons {
    /// Downloads and parses the component and dependency catalogs.
    ///
    /// Components are attempted first, then dependencies. A family failure other
    /// than cancellation does not prevent the other attempt. Each successful
    /// catalog is cached before a single snapshot is published; a failed family
    /// retains its previous catalog. The operation then reports
    /// [`CatalogError::Refresh`] if either attempt failed, after publishing any
    /// successful family.
    ///
    /// Cache writes occur in the same order. If a later write fails, earlier cache
    /// writes remain on disk but no new snapshot is published. Cancellation is
    /// checked through publication-lock acquisition; once cache writes begin, the
    /// operation runs through publication unless a write fails.
    ///
    /// # Errors
    ///
    /// The operation fails if cancelled before cache writes begin, either URL is
    /// missing, a download or schema parse fails, or a successful catalog cannot be
    /// written to its cache. Per-family URL, download, and parse failures are
    /// combined into [`CatalogError::Refresh`]; cache-write failures are returned
    /// directly.
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

    /// Downloads and parses one family catalog in temporary storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the family URL is absent, the operation is cancelled,
    /// the transfer or temporary storage fails, or the document does not match the
    /// current catalog schema.
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
        fs::with_temp_dir(&self.0.directories.staging(), |stage| async move {
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
