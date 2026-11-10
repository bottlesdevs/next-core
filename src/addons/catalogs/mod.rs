//! Remote catalogs and acquisition of their releases.
//!
//! [`Catalogs`] publishes immutable snapshots of the cached catalogs from every
//! configured source behind a cheap cloneable handle. Refreshes replace the
//! snapshot; fetching a release downloads it and commits it to [`Addons`].

mod fetch;
mod refresh;

use super::{
    Addons, RoleMarker,
    catalog::{Catalog, CatalogEntry},
};
use crate::{error::Result, utils::paths};
use download_manager::manager::{DownloadManager, DownloadManagerConfig};
use futures_core::Stream;
use http_client::HttpClient;
use std::{collections::HashMap, sync::Arc};
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
/// use bottles_core::{CatalogEntry, Catalogs, Runner};
///
/// fn supported_runners(catalogs: &Catalogs) -> Vec<CatalogEntry> {
///     catalogs
///         .state()
///         .entries::<Runner>()
///         .filter(|entry| entry.is_supported())
///         .cloned()
///         .collect()
/// }
/// ```
#[derive(Clone)]
pub struct Catalogs(Arc<CatalogsInner>);

struct CatalogsInner {
    http_client: Arc<dyn HttpClient>,
    downloader: DownloadManager,
    addons: Addons,
    sources: Vec<Url>,
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
    /// Cached catalogs in source order. Sources without a cache are absent.
    catalogs: Vec<(Url, Arc<Catalog>)>,
}

impl Catalogs {
    /// Opens the catalogs of `sources` from the cache, acquiring releases into `addons`.
    ///
    /// A missing cache is allowed, and cached catalogs of sources no longer
    /// configured are ignored.
    ///
    /// # Errors
    ///
    /// Returns an error if the download manager cannot initialize or a present
    /// cache cannot be read or parsed.
    pub(crate) async fn load(
        http_client: Arc<dyn HttpClient>,
        addons: Addons,
        sources: Vec<Url>,
    ) -> Result<Self> {
        if sources.is_empty() {
            tracing::warn!("no catalog sources are configured");
        }
        let mut cached = read_cache().await?;
        let state = CatalogsState {
            catalogs: sources
                .iter()
                .filter_map(|source| {
                    let catalog = cached.remove(source)?;
                    Some((source.clone(), Arc::new(catalog)))
                })
                .collect(),
        };
        Ok(Self(Arc::new(CatalogsInner {
            downloader: DownloadManager::new(
                http_client.clone(),
                DownloadManagerConfig::default(),
            )?,
            http_client,
            addons,
            sources,
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
    /// Returns entries with role `R` in source order, then catalog order.
    ///
    /// Use [`AddonRole`](super::AddonRole) as `R` to list entries of every role.
    /// The result is empty until a catalog has been loaded from cache or
    /// published by [`Catalogs::refresh`].
    pub fn entries<R: RoleMarker>(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.all().filter(|entry| R::try_from(entry.role()).is_ok())
    }

    /// Returns the entry with UUID `id` if it has role `R`.
    ///
    /// When several sources list `id`, the first source wins.
    pub fn entry<R: RoleMarker>(&self, id: Uuid) -> Option<&CatalogEntry> {
        self.all()
            .find(|entry| entry.id() == id)
            .filter(|entry| R::try_from(entry.role()).is_ok())
    }

    fn all(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.catalogs
            .iter()
            .flat_map(|(_, catalog)| catalog.entries())
    }
}

/// Reads the cached catalogs keyed by source. A missing cache is empty.
async fn read_cache() -> Result<HashMap<Url, Catalog>> {
    match async_fs::read(paths::catalogs()).await {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(error) => Err(error.into()),
    }
}

/// Replaces the cache with `catalogs`.
async fn write_cache(catalogs: &[(Url, Arc<Catalog>)]) -> Result<()> {
    let cache: HashMap<&Url, &Catalog> = catalogs
        .iter()
        .map(|(source, catalog)| (source, catalog.as_ref()))
        .collect();
    async_fs::write(paths::catalogs(), serde_json::to_vec(&cache)?).await?;
    Ok(())
}
