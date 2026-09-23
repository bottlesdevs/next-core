//! Shared catalog and acquired-release state.
//!
//! [`Addons`] publishes immutable snapshots behind a cheap cloneable handle.
//! Catalog refreshes, release commits, and removals replace the snapshot; downloads
//! happen outside the publication lock.

mod acquire;
mod download;
mod refresh;
mod storage;

use super::{
    Addon, Component, Dependency,
    catalog::{Catalog, CatalogEntry, CatalogUrls},
};
use crate::{Directories, error::Result};
use download_manager::manager::DownloadManager;
use futures_core::Stream;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use url::Url;
use uuid::Uuid;

/// Provides access to addon catalogs and acquired releases.
///
/// Catalog entries describe remote releases, while [`Addon`] values describe
/// releases already stored locally. Fetching a catalog entry only acquires its
/// payload; it does not select the addon for any environment.
///
/// Cloned managers share the same state. Values returned from query methods are
/// snapshots and remain unchanged after refreshes or storage changes. Query again,
/// or subscribe with [`watch`](Self::watch), to observe a later snapshot.
///
/// # Examples
///
/// ```
/// use bottles_core::{Addons, CatalogEntry, Component, Slot};
///
/// fn supported_runners(addons: &Addons) -> Vec<CatalogEntry<Component>> {
///     addons
///         .component_entries()
///         .into_iter()
///         .filter(|entry| entry.slot() == Slot::Runner && entry.is_supported())
///         .collect()
/// }
/// ```
#[derive(Clone)]
pub struct Addons(Arc<AddonsInner>);

struct AddonsInner {
    directories: Directories,
    downloader: Arc<DownloadManager>,
    catalog_urls: CatalogUrls,
    published: watch::Sender<Arc<AddonsState>>,
    /// Serializes filesystem commits and state publication, not transfers.
    write: Mutex<()>,
}

#[derive(Clone, Debug, Default)]
struct AddonsState {
    component_catalog: Option<Arc<Catalog<Component>>>,
    dependency_catalog: Option<Arc<Catalog<Dependency>>>,
    components: HashMap<Uuid, Arc<Addon<Component>>>,
    dependencies: HashMap<Uuid, Arc<Addon<Dependency>>>,
}

impl AddonsState {
    fn contains(&self, id: Uuid) -> bool {
        self.components.contains_key(&id) || self.dependencies.contains_key(&id)
    }
}

impl Addons {
    /// Opens the manager from cached catalogs and local release manifests.
    ///
    /// A missing catalog cache is allowed. Release manifests are loaded independently
    /// of payload contents, so a returned manager can still contain a record whose
    /// payload was modified outside the manager.
    ///
    /// # Errors
    ///
    /// Returns an error if a present catalog or release manifest cannot be read or
    /// parsed, a manifest UUID differs from its directory name, or duplicate UUIDs
    /// are found across addon families.
    pub(crate) async fn load(
        directories: Directories,
        downloader: Arc<DownloadManager>,
        component_catalog_url: Option<Url>,
        dependency_catalog_url: Option<Url>,
    ) -> Result<Self> {
        let state = AddonsState::load_cached(&directories).await?;
        let (published, _) = watch::channel(Arc::new(state));
        Ok(Self(Arc::new(AddonsInner {
            directories,
            downloader,
            catalog_urls: CatalogUrls {
                components: component_catalog_url,
                dependencies: dependency_catalog_url,
            },
            published,
            write: Mutex::new(()),
        })))
    }

    /// Returns component entries in their current catalog order.
    ///
    /// The result is empty until a component catalog has been loaded from cache or
    /// published by [`refresh`](Self::refresh).
    pub fn component_entries(&self) -> Vec<CatalogEntry<Component>> {
        self.state()
            .component_catalog
            .iter()
            .flat_map(|catalog| catalog.entries().iter().cloned())
            .collect()
    }

    /// Returns dependency entries in their current catalog order.
    ///
    /// The result is empty until a dependency catalog has been loaded from cache or
    /// published by [`refresh`](Self::refresh).
    pub fn dependency_entries(&self) -> Vec<CatalogEntry<Dependency>> {
        self.state()
            .dependency_catalog
            .iter()
            .flat_map(|catalog| catalog.entries().iter().cloned())
            .collect()
    }

    /// Returns all locally acquired component releases.
    ///
    /// The order is unspecified.
    pub fn components(&self) -> Vec<Arc<Addon<Component>>> {
        self.state().components.values().cloned().collect()
    }

    /// Returns all locally acquired dependency releases.
    ///
    /// The result order is unspecified.
    pub fn dependencies(&self) -> Vec<Arc<Addon<Dependency>>> {
        self.state().dependencies.values().cloned().collect()
    }

    /// Returns the locally acquired component with UUID `id`, if present.
    pub fn component(&self, id: Uuid) -> Option<Arc<Addon<Component>>> {
        self.state().components.get(&id).cloned()
    }

    /// Returns the locally acquired dependency with UUID `id`, if present.
    pub fn dependency(&self, id: Uuid) -> Option<Arc<Addon<Dependency>>> {
        self.state().dependencies.get(&id).cloned()
    }

    /// Returns the component catalog entry with UUID `id`.
    ///
    /// Returns `None` when no valid component catalog is loaded or the release
    /// is absent from it.
    pub fn component_entry(&self, id: Uuid) -> Option<CatalogEntry<Component>> {
        self.state()
            .component_catalog
            .as_ref()
            .and_then(|catalog| catalog.entry(id))
            .cloned()
    }

    /// Returns the dependency catalog entry with UUID `id`.
    ///
    /// Returns `None` when no valid dependency catalog is loaded or the release
    /// is absent from it.
    pub fn dependency_entry(&self, id: Uuid) -> Option<CatalogEntry<Dependency>> {
        self.state()
            .dependency_catalog
            .as_ref()
            .and_then(|catalog| catalog.entry(id))
            .cloned()
    }

    /// Returns a stream that observes published addon-state changes.
    ///
    /// The first item is available immediately. Later items follow successful
    /// catalog refreshes, release acquisitions, imports, and removals. Slow
    /// consumers may observe several publications as one item. Each item is a
    /// manager handle; call its query methods to read the current snapshot.
    pub fn watch(&self) -> impl Stream<Item = Self> + Send + 'static + use<> {
        let addons = self.clone();
        tokio_stream::StreamExt::map(WatchStream::new(self.0.published.subscribe()), move |_| {
            addons.clone()
        })
    }

    fn state(&self) -> Arc<AddonsState> {
        self.0.published.borrow().clone()
    }

    /// Publishes the already committed local snapshot without filesystem discovery.
    fn publish(&self, state: AddonsState) {
        self.0.published.send_replace(Arc::new(state));
    }
}
