//! Remote catalog refresh and publication.

use std::{io, sync::Arc};

use futures_lite::AsyncReadExt;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    Operation, Progress, Stage,
    error::{Error, Result},
    utils::paths,
};

use super::super::{CatalogError, catalog::Catalog};
use super::Catalogs;

impl Catalogs {
    /// Downloads and parses the component and dependency catalogs.
    ///
    /// Components are attempted first, then dependencies. A source failure other
    /// than cancellation does not prevent the other attempt. Each successful
    /// catalog is cached before a single snapshot is published; a failed source
    /// retains its previous catalog. The operation then reports
    /// [`CatalogError::Refresh`] if either attempt failed, after publishing any
    /// successful source.
    ///
    /// Cache writes occur in the same order. If a later write fails, earlier cache
    /// writes remain on disk but no new snapshot is published. Cancellation is
    /// checked through publication-lock acquisition; once cache writes begin, the
    /// operation runs through publication unless a write fails.
    ///
    /// # Errors
    ///
    /// The operation fails if cancelled before cache writes begin, either URL is
    /// missing, a request or schema parse fails, or a successful catalog cannot be
    /// written to its cache. Per-source URL, request, and parse failures are
    /// combined into [`CatalogError::Refresh`]; cache-write failures are returned
    /// directly.
    pub fn refresh(&self) -> Operation<()> {
        let catalogs = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let component = catalogs
                .request(
                    catalogs.0.component_catalog_url.clone(),
                    "components",
                    &progress,
                    &cancellation,
                )
                .await;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let dependency = catalogs
                .request(
                    catalogs.0.dependency_catalog_url.clone(),
                    "dependencies",
                    &progress,
                    &cancellation,
                )
                .await;

            let _write = cancellation
                .run_until_cancelled(catalogs.0.write.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let current = catalogs.state();
            let component_catalog = match &component {
                Ok(catalog) => {
                    catalog.save(&paths::component_catalog()).await?;
                    Some(catalog.clone())
                }
                Err(_) => current.component_catalog.clone(),
            };
            let dependency_catalog = match &dependency {
                Ok(catalog) => {
                    catalog.save(&paths::dependency_catalog()).await?;
                    Some(catalog.clone())
                }
                Err(_) => current.dependency_catalog.clone(),
            };
            catalogs.publish(super::CatalogsState {
                component_catalog,
                dependency_catalog,
            });

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

    /// Requests and parses one catalog in memory.
    async fn request(
        &self,
        url: Option<Url>,
        label: &'static str,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<Arc<Catalog>> {
        let url = url.ok_or(CatalogError::UrlNotConfigured(label))?;
        progress.send_replace(Some(Progress::new(Stage::Downloading {
            file: format!("{label} catalog"),
        })));
        let request = http::Request::get(url.as_str())
            .body(Vec::new())
            .map_err(io::Error::other)?;
        let body = async {
            let response = self
                .0
                .http_client
                .send(request)
                .await
                .map_err(io::Error::other)?;
            let mut body = Vec::new();
            response.into_body().read_to_end(&mut body).await?;
            Ok::<_, io::Error>(body)
        };
        let body = cancellation
            .run_until_cancelled(body)
            .await
            .ok_or(Error::Cancelled)??;
        Ok(Arc::new(serde_json::from_slice(&body)?))
    }
}
