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
    Addon, AddonRole, RoleMarker,
    catalog::{Catalog, CatalogEntry},
};
use crate::{Directories, error::Result};
use download_manager::manager::DownloadManager;
use futures_core::Stream;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use url::Url;
use uuid::Uuid;

/// Keeps catalog discovery and release acquisition separate from environment selection.
///
/// Catalog entries describe remote releases, while [`Addon`] values describe
/// releases already stored locally. Fetching a catalog entry only acquires its
/// payload; it does not select the addon for any environment.
///
/// Cloned managers share the same state. Read an immutable snapshot with
/// [`state`](Self::state), or subscribe with [`watch`](Self::watch) to observe
/// later snapshots.
///
/// # Examples
///
/// ```
/// use bottles_core::{AddonRole, Addons, CatalogEntry};
///
/// fn supported_runners(addons: &Addons) -> Vec<CatalogEntry> {
///     addons
///         .state()
///         .component_entries()
///         .into_iter()
///         .filter(|entry| matches!(entry.role(), AddonRole::Runner(_)) && entry.is_supported())
///         .collect()
/// }
/// ```
#[derive(Clone)]
pub struct Addons(Arc<AddonsInner>);

struct AddonsInner {
    directories: Directories,
    downloader: Arc<DownloadManager>,
    component_catalog_url: Option<Url>,
    dependency_catalog_url: Option<Url>,
    published: watch::Sender<Arc<AddonsState>>,
    /// Serializes filesystem commits and state publication, not transfers.
    write: Mutex<()>,
}

/// An immutable snapshot of cached catalogs and locally acquired releases.
///
/// Obtained through [`Addons::state`] or [`Addons::watch`]. Later publications do
/// not change this snapshot, and retaining it does not keep the manager alive.
#[derive(Clone, Debug, Default)]
pub struct AddonsState {
    component_catalog: Option<Arc<Catalog>>,
    dependency_catalog: Option<Arc<Catalog>>,
    releases: HashMap<Uuid, Addon<AddonRole>>,
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
            component_catalog_url,
            dependency_catalog_url,
            published,
            write: Mutex::new(()),
        })))
    }

    /// Returns the current immutable snapshot.
    pub fn state(&self) -> Arc<AddonsState> {
        self.0.published.borrow().clone()
    }

    /// Returns a stream of immutable addon-state snapshots.
    ///
    /// The first item is available immediately. Later items follow refreshes that
    /// reach publication, including refreshes that report per-source failures, and
    /// successful new release acquisitions, imports, and removals. Reusing an
    /// already-acquired release does not publish. Slow consumers may observe several
    /// publications as one item. The stream does not keep the manager alive and
    /// ends after the last manager handle is dropped, including those held by
    /// operations.
    pub fn watch(&self) -> impl Stream<Item = Arc<AddonsState>> + Send + 'static + use<> {
        WatchStream::new(self.0.published.subscribe())
    }

    /// Publishes the already committed local snapshot without filesystem discovery.
    fn publish(&self, state: AddonsState) {
        self.0.published.send_replace(Arc::new(state));
    }
}

impl AddonsState {
    /// Returns all locally acquired releases with role `R` in unspecified order.
    ///
    /// Use [`AddonRole`] as `R` to list releases of every role.
    pub fn releases<R: RoleMarker>(&self) -> Vec<Addon<R>> {
        self.releases
            .values()
            .filter_map(|release| release.clone().cast())
            .collect()
    }

    /// Returns the locally acquired release with UUID `id` if it has role `R`.
    pub fn release<R: RoleMarker>(&self, id: Uuid) -> Option<Addon<R>> {
        self.releases.get(&id)?.clone().cast()
    }

    /// Returns runner, `WineBridge`, UMU, and prefix-component entries in catalog order.
    ///
    /// The result is empty until the component catalog has been loaded from cache or
    /// published by [`Addons::refresh`].
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
    /// published by [`Addons::refresh`].
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
