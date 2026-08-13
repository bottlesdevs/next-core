//! Catalog refresh and cache replacement.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    Operation, Progress, Stage,
    error::{Error, Result},
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
            let dependency = addons
                .download_catalog::<Dependency>(progress, &cancellation)
                .await;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }

            let _write = addons.0.write.lock().await;
            let current = addons.state();
            let component_catalog = match &component {
                Ok(catalog) => {
                    catalog.save(addons.0.context.directories()).await?;
                    Some(catalog.clone())
                }
                Err(_) => current.components.catalog.clone(),
            };
            let dependency_catalog = match &dependency {
                Ok(catalog) => {
                    catalog.save(addons.0.context.directories()).await?;
                    Some(catalog.clone())
                }
                Err(_) => current.dependencies.catalog.clone(),
            };
            addons
                .publish(component_catalog, dependency_catalog)
                .await?;

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
        let staging = self.0.context.directories().data_dir().join(".staging");
        async_fs::create_dir_all(&staging).await?;
        let downloaded = staging.join(format!("catalog-{}.json", Uuid::new_v4()));
        let result = async {
            download(
                self.0.context.downloader(),
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
        }
        .await;
        let _ = async_fs::remove_file(downloaded).await;
        result
    }
}
