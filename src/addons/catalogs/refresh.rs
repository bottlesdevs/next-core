//! Remote catalog refresh and publication.

use std::{io, sync::Arc};

use futures_lite::AsyncReadExt;
use url::Url;

use crate::{Job, Operation, Stage, error::Result};

use super::super::{CatalogError, catalog::Catalog};
use super::{Catalogs, CatalogsState, write_cache};

impl Catalogs {
    /// Downloads and parses the catalog of every configured source.
    ///
    /// Sources are requested concurrently. A failed source retains its previous
    /// catalog and does not prevent the others from updating. The cache is
    /// written once and a single snapshot is published before the operation
    /// reports [`CatalogError::Refresh`] for the sources that failed. Without
    /// sources, this succeeds without publishing.
    ///
    /// Cancellation is checked through publication-lock acquisition; once the
    /// cache write begins, the operation runs through publication unless the
    /// write fails.
    ///
    /// # Errors
    ///
    /// The operation fails if cancelled before the cache write begins, a
    /// request or schema parse fails, or the cache cannot be written. Per-source
    /// request and parse failures are combined into [`CatalogError::Refresh`];
    /// cache-write failures are returned directly.
    pub fn refresh(&self) -> Operation<()> {
        let catalogs = self.clone();
        Operation::new(move |job| async move {
            let sources = &catalogs.0.sources;
            if sources.is_empty() {
                return Ok(());
            }
            job.transition(Stage::Downloading {
                file: "catalogs".into(),
            })?;
            let requested = futures_util::future::join_all(
                sources.iter().map(|source| catalogs.request(source, &job)),
            )
            .await;

            let _write = job.cancellable(catalogs.0.write.lock()).await?;
            let current = catalogs.state();
            let mut next = Vec::new();
            let mut failures = Vec::new();
            for (source, requested) in sources.iter().zip(requested) {
                match requested {
                    Ok(catalog) => next.push((source.clone(), catalog)),
                    Err(error) => {
                        failures.push((source.clone(), error.to_string()));
                        next.extend(
                            current
                                .catalogs
                                .iter()
                                .find(|(cached, _)| cached == source)
                                .cloned(),
                        );
                    }
                }
            }
            write_cache(&next).await?;
            catalogs.publish(CatalogsState { catalogs: next });

            if failures.is_empty() {
                Ok(())
            } else {
                Err(CatalogError::Refresh(failures).into())
            }
        })
    }

    /// Requests and parses one catalog in memory.
    async fn request(&self, source: &Url, job: &Job) -> Result<Arc<Catalog>> {
        let request = http::Request::get(source.as_str())
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
        let body = job.cancellable(body).await??;
        Ok(Arc::new(serde_json::from_slice(&body)?))
    }
}
