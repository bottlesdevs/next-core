//! Remote catalogs and acquisition of their releases.
//!
//! [`Catalogs`] publishes immutable snapshots of the cached component and
//! dependency catalogs behind a cheap cloneable handle. Refreshes replace the
//! snapshot; fetching a release downloads it and commits it to [`Addons`].

mod download;
mod fetch;
mod refresh;

use super::{
    AddonRole, Addons,
    catalog::{Catalog, CatalogEntry},
};
use crate::{error::Result, utils::paths};
use download_manager::manager::{DownloadManager, DownloadManagerConfig};
use futures_core::Stream;
use http_client::HttpClient;
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use url::Url;
use uuid::Uuid;

/// Discovers remote releases and acquires them into [`Addons`].
///
/// Catalog entries describe remote releases. Fetching an entry only acquires
/// its payload; it does not select the addon for any prefix. Cloned handles
/// share the same state. Read an immutable snapshot with
/// [`state`](Self::state), or subscribe with [`watch`](Self::watch) to observe
/// later snapshots.
///
/// # Examples
///
/// ```
/// use bottles_core::{AddonRole, CatalogEntry, Catalogs};
///
/// fn supported_runners(catalogs: &Catalogs) -> Vec<CatalogEntry> {
///     catalogs
///         .state()
///         .component_entries()
///         .into_iter()
///         .filter(|entry| matches!(entry.role(), AddonRole::Runner(_)) && entry.is_supported())
///         .collect()
/// }
/// ```
#[derive(Clone)]
pub struct Catalogs(Arc<CatalogsInner>);

struct CatalogsInner {
    http_client: Arc<dyn HttpClient>,
    downloader: DownloadManager,
    addons: Addons,
    component_catalog_url: Option<Url>,
    dependency_catalog_url: Option<Url>,
    published: watch::Sender<Arc<CatalogsState>>,
    /// Serializes cache writes and state publication, not transfers.
    write: Mutex<()>,
}

/// An immutable snapshot of the cached catalogs.
///
/// Obtained through [`Catalogs::state`] or [`Catalogs::watch`]. Later
/// publications do not change this snapshot.
#[derive(Clone, Debug, Default)]
pub struct CatalogsState {
    component_catalog: Option<Arc<Catalog>>,
    dependency_catalog: Option<Arc<Catalog>>,
}

impl Catalogs {
    /// Opens the catalogs from their caches, acquiring releases into `addons`.
    ///
    /// A missing catalog cache is allowed.
    ///
    /// # Errors
    ///
    /// Returns an error if the download manager cannot initialize or a present
    /// catalog cache cannot be read or parsed.
    pub(crate) async fn load(
        http_client: Arc<dyn HttpClient>,
        addons: Addons,
        component_catalog_url: Option<Url>,
        dependency_catalog_url: Option<Url>,
    ) -> Result<Self> {
        let state = CatalogsState {
            component_catalog: Catalog::load(&paths::component_catalog()).await?,
            dependency_catalog: Catalog::load(&paths::dependency_catalog()).await?,
        };
        Ok(Self(Arc::new(CatalogsInner {
            downloader: DownloadManager::new(
                http_client.clone(),
                DownloadManagerConfig::default(),
            )?,
            http_client,
            addons,
            component_catalog_url,
            dependency_catalog_url,
            published: watch::channel(Arc::new(state)).0,
            write: Mutex::new(()),
        })))
    }

    /// Returns the current immutable snapshot.
    pub fn state(&self) -> Arc<CatalogsState> {
        self.0.published.borrow().clone()
    }

    /// Returns a stream of immutable catalog snapshots.
    ///
    /// The first item is available immediately. Later items follow refreshes
    /// that reach publication, including refreshes that report per-source
    /// failures. Slow consumers may observe several publications as one item.
    /// The stream ends after the last handle is dropped, including those held
    /// by operations.
    pub fn watch(&self) -> impl Stream<Item = Arc<CatalogsState>> + Send + 'static + use<> {
        WatchStream::new(self.0.published.subscribe())
    }

    /// Cancels downloads and stops the background download service.
    pub(crate) async fn shutdown(&self) {
        self.0.downloader.shutdown().await;
    }

    fn publish(&self, state: CatalogsState) {
        self.0.published.send_replace(Arc::new(state));
    }
}

impl CatalogsState {
    /// Returns runner, `WineBridge`, UMU, and prefix-component entries in catalog order.
    ///
    /// The result is empty until the component catalog has been loaded from cache or
    /// published by [`Catalogs::refresh`].
    pub fn component_entries(&self) -> Vec<CatalogEntry> {
        self.component_catalog
            .iter()
            .flat_map(|catalog| catalog.entries())
            .cloned()
            .collect()
    }

    /// Returns dependency entries in their current catalog order.
    ///
    /// The result is empty until a dependency catalog has been loaded from cache or
    /// published by [`Catalogs::refresh`].
    pub fn dependency_entries(&self) -> Vec<CatalogEntry> {
        self.dependency_catalog
            .iter()
            .flat_map(|catalog| catalog.entries())
            .filter(|entry| matches!(entry.role(), AddonRole::Dependency(_)))
            .cloned()
            .collect()
    }

    /// Returns the entry with UUID `id` from the mixed component catalog.
    ///
    /// Returns `None` when no valid component catalog is loaded or the release
    /// is absent from it.
    pub fn component_entry(&self, id: Uuid) -> Option<CatalogEntry> {
        self.component_catalog
            .as_ref()
            .and_then(|catalog| catalog.entry(id))
            .cloned()
    }

    /// Returns the dependency catalog entry with UUID `id`.
    ///
    /// Returns `None` when no valid dependency catalog is loaded or the release
    /// is absent from it.
    pub fn dependency_entry(&self, id: Uuid) -> Option<CatalogEntry> {
        self.dependency_catalog
            .as_ref()
            .and_then(|catalog| catalog.entry(id))
            .filter(|entry| matches!(entry.role(), AddonRole::Dependency(_)))
            .cloned()
    }
}
