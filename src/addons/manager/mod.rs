//! Shared addon ownership, queries, and publication.

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

/// The shared manager for addon catalogs and local storage.
///
/// Remote releases are exposed as [`CatalogEntry`] values. Fetching one adds an
/// [`Addon`] and its payload to shared storage. Bottles and standalone programs
/// clone that complete record when selecting a component or installing a dependency.
/// Fetching alone does not modify any bottle.
///
/// Clones refer to the same manager state. Returned [`CatalogEntry`] values and
/// [`Addon`] handles are snapshots: they do not change after a refresh,
/// fetch, or removal. Query the manager again, or use [`watch`](Self::watch), to
/// observe a later publication.
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
    /// Loads cached catalogs and frozen local records independently of payload files.
    ///
    /// Missing catalog caches are optional; read and parse failures are returned.
    /// Invalid or incomplete records are returned as errors. A known record does
    /// not guarantee its payload is available; installation and runtime operations
    /// access the inputs they require directly.
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

    /// Returns the component releases in current catalog order.
    ///
    /// The result is empty when no valid component catalog has been loaded.
    pub fn component_entries(&self) -> Vec<CatalogEntry<Component>> {
        self.state()
            .component_catalog
            .iter()
            .flat_map(|catalog| catalog.entries().iter().cloned())
            .collect()
    }

    /// Returns the dependency releases in current catalog order.
    ///
    /// The result is empty when no valid dependency catalog has been loaded.
    pub fn dependency_entries(&self) -> Vec<CatalogEntry<Dependency>> {
        self.state()
            .dependency_catalog
            .iter()
            .flat_map(|catalog| catalog.entries().iter().cloned())
            .collect()
    }

    /// Returns downloaded or imported component releases.
    ///
    /// The order is unspecified.
    pub fn components(&self) -> Vec<Arc<Addon<Component>>> {
        self.state().components.values().cloned().collect()
    }

    /// Returns downloaded dependency releases.
    /// The order is unspecified.
    pub fn dependencies(&self) -> Vec<Arc<Addon<Dependency>>> {
        self.state().dependencies.values().cloned().collect()
    }

    /// Returns the known component with this release identifier.
    pub fn component(&self, id: Uuid) -> Option<Arc<Addon<Component>>> {
        self.state().components.get(&id).cloned()
    }

    /// Returns the known dependency with this release identifier.
    pub fn dependency(&self, id: Uuid) -> Option<Arc<Addon<Dependency>>> {
        self.state().dependencies.get(&id).cloned()
    }

    /// Returns the current component catalog entry with this identifier.
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

    /// Returns the current dependency catalog entry with this identifier.
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

    /// Watches changes to catalogs and local releases.
    ///
    /// The stream yields immediately and may coalesce publications for slow
    /// consumers. Each value is a live manager handle; query it for current data.
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
