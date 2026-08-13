//! Shared addon catalogs, local discovery, downloads, and removal.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use download_manager::{events::Progress as DownloadProgress, manager::DownloadManager};
use futures_core::Stream;
use futures_util::{FutureExt, StreamExt};
use semver::Version;
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use super::{
    AddonError, Component, Dependency, IndexEntry, Slot,
    catalog::{Catalog, CatalogEntry, CatalogUrls},
    index::AddonIndex,
};
use crate::{
    Context, Directories, Transfer,
    error::{Error, Result},
};

mod catalog;
mod fetch;

/// A shared snapshot-based view of catalog and hand-placed addons.
///
/// Values returned by collection methods do not update after a refresh, fetch,
/// or removal. Query the manager again, or use [`watch`](Self::watch), to observe
/// a later publication.
#[derive(Clone)]
pub struct Addons(Arc<AddonsInner>);

struct AddonsInner {
    context: Context,
    catalog_urls: CatalogUrls,
    published: watch::Sender<Arc<AddonsState>>,
    /// Serializes filesystem commits and state publication, not transfers.
    write: Mutex<()>,
}

impl Addons {
    pub(crate) async fn load(
        context: Context,
        component_catalog_url: Option<Url>,
        dependency_catalog_url: Option<Url>,
    ) -> Result<Self> {
        let state = AddonsState::load_cached(context.directories()).await?;
        let (published, _) = watch::channel(Arc::new(state));
        Ok(Self(Arc::new(AddonsInner {
            context,
            catalog_urls: CatalogUrls {
                components: component_catalog_url,
                dependencies: dependency_catalog_url,
            },
            published,
            write: Mutex::new(()),
        })))
    }

    /// Returns all releases in the current component catalog.
    pub fn component_entries(&self) -> Vec<CatalogEntry<Component>> {
        self.state()
            .components
            .catalog
            .iter()
            .flat_map(|catalog| catalog.entries().iter().cloned())
            .collect()
    }

    /// Returns all releases in the current dependency catalog.
    pub fn dependency_entries(&self) -> Vec<CatalogEntry<Dependency>> {
        self.state()
            .dependencies
            .catalog
            .iter()
            .flat_map(|catalog| catalog.entries().iter().cloned())
            .collect()
    }

    /// Returns downloaded and hand-placed components.
    pub fn components(&self) -> Vec<Arc<IndexEntry<Component>>> {
        self.state().components.addons.values().cloned().collect()
    }

    /// Returns complete downloaded dependencies.
    pub fn dependencies(&self) -> Vec<Arc<IndexEntry<Dependency>>> {
        self.state().dependencies.addons.values().cloned().collect()
    }

    /// Returns the local component with this immutable release identifier.
    pub fn component(&self, id: Uuid) -> Option<Arc<IndexEntry<Component>>> {
        self.state().components.addons.get(&id).cloned()
    }

    /// Returns the local dependency with this immutable release identifier.
    pub fn dependency(&self, id: Uuid) -> Option<Arc<IndexEntry<Dependency>>> {
        self.state().dependencies.addons.get(&id).cloned()
    }

    /// Returns the current component catalog entry with this identifier.
    pub fn component_entry(&self, id: Uuid) -> Option<CatalogEntry<Component>> {
        self.state()
            .components
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.entry(id))
            .cloned()
    }

    /// Returns the current dependency catalog entry with this identifier.
    pub fn dependency_entry(&self, id: Uuid) -> Option<CatalogEntry<Dependency>> {
        self.state()
            .dependencies
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.entry(id))
            .cloned()
    }

    /// Watches state publications.
    ///
    /// The stream yields immediately and may coalesce publications for slow
    /// consumers. Each value is a live manager handle; query it for current data.
    pub fn watch(&self) -> impl Stream<Item = Self> + Send + 'static {
        let addons = self.clone();
        tokio_stream::StreamExt::map(WatchStream::new(self.0.published.subscribe()), move |_| {
            addons.clone()
        })
    }

    /// Removes a component from shared storage without checking bottle references.
    pub async fn remove_component(&self, id: Uuid) -> Result<()> {
        let _write = self.0.write.lock().await;
        let state = self.state();
        let component = state
            .components
            .addons
            .get(&id)
            .ok_or(AddonError::NotFound(id))?;
        async_fs::remove_dir_all(component.path(self.0.context.directories())).await?;
        let mut next = state.components.clone();
        next.addons.remove(&id);
        next.save(self.0.context.directories()).await?;
        self.publish(
            state.components.catalog.clone(),
            state.dependencies.catalog.clone(),
        )
        .await
    }

    /// Removes a dependency from shared storage without checking bottle references.
    pub async fn remove_dependency(&self, id: Uuid) -> Result<()> {
        let _write = self.0.write.lock().await;
        let state = self.state();
        let dependency = state
            .dependencies
            .addons
            .get(&id)
            .ok_or(AddonError::NotFound(id))?;
        async_fs::remove_dir_all(dependency.path(self.0.context.directories())).await?;
        let mut next = state.dependencies.clone();
        next.addons.remove(&id);
        next.save(self.0.context.directories()).await?;
        self.publish(
            state.components.catalog.clone(),
            state.dependencies.catalog.clone(),
        )
        .await
    }

    pub(crate) fn latest_component(&self, slot: Slot) -> Option<Arc<IndexEntry<Component>>> {
        self.state()
            .components
            .addons
            .values()
            .filter(|component| component.slot() == slot)
            .max_by_key(|component| {
                Version::parse(component.version())
                    .expect("selected component versions are semantic")
            })
            .cloned()
    }

    fn state(&self) -> Arc<AddonsState> {
        self.0.published.borrow().clone()
    }

    async fn create_stage(&self) -> Result<PathBuf> {
        let staging = self.0.context.directories().data_dir().join(".staging");
        async_fs::create_dir_all(&staging).await?;
        let stage = staging.join(Uuid::new_v4().to_string());
        async_fs::create_dir_all(&stage).await?;
        Ok(stage)
    }

    async fn publish(
        &self,
        component_catalog: Option<Arc<Catalog<Component>>>,
        dependency_catalog: Option<Arc<Catalog<Dependency>>>,
    ) -> Result<()> {
        let state = AddonsState::load(
            component_catalog,
            dependency_catalog,
            self.0.context.directories(),
        )
        .await?;
        self.0.published.send_replace(Arc::new(state));
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct AddonsState {
    components: AddonIndex<Component>,
    dependencies: AddonIndex<Dependency>,
}

impl AddonsState {
    async fn load_cached(directories: &Directories) -> Result<Self> {
        let component_catalog = Catalog::<Component>::load(directories).await;
        let dependency_catalog = Catalog::<Dependency>::load(directories).await;
        Self::load(component_catalog, dependency_catalog, directories).await
    }

    async fn load(
        component_catalog: Option<Arc<Catalog<Component>>>,
        dependency_catalog: Option<Arc<Catalog<Dependency>>>,
        directories: &Directories,
    ) -> Result<Self> {
        let components = AddonIndex::<Component>::load(directories).await?;
        let components = match component_catalog {
            Some(catalog) => components.with_catalog(catalog),
            None => components,
        };
        let dependencies = AddonIndex::<Dependency>::load(directories).await?;
        let dependencies = match dependency_catalog {
            Some(catalog) => dependencies.with_catalog(catalog),
            None => dependencies,
        };
        Ok(Self {
            components,
            dependencies,
        })
    }
}

async fn download(
    downloader: &DownloadManager,
    url: Url,
    destination: &Path,
    cancellation: &CancellationToken,
    mut on_progress: impl FnMut(Transfer),
) -> Result<()> {
    let download = downloader.download(url, destination)?;
    let mut updates = Box::pin(
        download
            .progress()
            .chain(futures_util::stream::pending::<DownloadProgress>()),
    );
    let result = download.clone().fuse();
    let cancelled = cancellation.cancelled().fuse();
    futures_util::pin_mut!(result, cancelled);

    loop {
        futures_util::select_biased! {
            result = result => {
                result?;
                return Ok(());
            },
            _ = cancelled => {
                download.cancel().await?;
                return Err(Error::Cancelled);
            }
            update = updates.next().fuse() => {
                let update = update.expect("progress stream is chained with pending");
                on_progress(Transfer {
                    current: update.bytes_downloaded(),
                    total: update.total_bytes(),
                });
            }
        }
    }
}
