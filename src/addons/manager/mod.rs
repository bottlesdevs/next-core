//! Shared addon state, queries, publication, and storage removal.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use download_manager::{events::Progress as DownloadProgress, manager::DownloadManager};
use futures_core::Stream;
use futures_util::{FutureExt, StreamExt, TryStreamExt};
use semver::Version;
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use super::{
    AddonError, Component, Dependency, Release, Slot,
    catalog::{Catalog, CatalogEntry, CatalogUrls},
};
use crate::{
    Context, Directories, Transfer,
    error::{Error, Result},
};

mod catalog;
mod fetch;
mod import;

/// The shared manager for addon catalogs and local storage.
///
/// Remote releases are exposed as [`CatalogEntry`] values. Fetching one adds an
/// [`Release`] to shared storage; bottles then persist an artifact-free
/// [`Addon`](crate::Addon) when selecting a component or installing a dependency.
/// Fetching alone does not modify any bottle.
///
/// Clones refer to the same manager state. Returned [`CatalogEntry`] values and
/// [`Release`] handles are snapshots: they do not change after a refresh,
/// fetch, or removal. Query the manager again, or use [`watch`](Self::watch), to
/// observe a later publication.
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
    /// Loads cached catalogs and complete local releases.
    ///
    /// An unavailable or invalid catalog cache is ignored. Invalid or incomplete
    /// releases are returned as errors.
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
    pub fn components(&self) -> Vec<Arc<Release<Component>>> {
        self.state().components.values().cloned().collect()
    }

    /// Returns downloaded dependency releases.
    /// The order is unspecified.
    pub fn dependencies(&self) -> Vec<Arc<Release<Dependency>>> {
        self.state().dependencies.values().cloned().collect()
    }

    /// Returns the known component with this release identifier.
    pub fn component(&self, id: Uuid) -> Option<Arc<Release<Component>>> {
        self.state().components.get(&id).cloned()
    }

    /// Returns the known dependency with this release identifier.
    pub fn dependency(&self, id: Uuid) -> Option<Arc<Release<Dependency>>> {
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

    /// Removes a component's entire release directory. Built Virgo artifacts remain.
    /// Does not stop environments or change their selections.
    pub async fn remove_component(&self, id: Uuid) -> Result<()> {
        let stage = {
            let _write = self.0.write.lock().await;
            let mut next = self.state().as_ref().clone();
            let release = next
                .components
                .remove(&id)
                .ok_or(AddonError::NotFound(id))?;
            let stage = self
                .withdraw_release(&release.directory(self.0.context.directories()))
                .await?;
            self.publish(next);
            stage
        };
        // Cleanup failure leaves only unpublished staging data.
        Ok(async_fs::remove_dir_all(stage).await?)
    }

    /// Removes a dependency's entire release directory. Built Virgo artifacts remain.
    /// Does not stop environments or change their selections.
    pub async fn remove_dependency(&self, id: Uuid) -> Result<()> {
        let stage = {
            let _write = self.0.write.lock().await;
            let mut next = self.state().as_ref().clone();
            let release = next
                .dependencies
                .remove(&id)
                .ok_or(AddonError::NotFound(id))?;
            let stage = self
                .withdraw_release(&release.directory(self.0.context.directories()))
                .await?;
            self.publish(next);
            stage
        };
        // Cleanup failure leaves only unpublished staging data.
        Ok(async_fs::remove_dir_all(stage).await?)
    }

    // Caller holds the manager write lock until the new snapshot is published.
    async fn withdraw_release(&self, path: &Path) -> Result<PathBuf> {
        let stage = self.create_stage().await?;
        match async_fs::rename(path, stage.join("release")).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let _ = async_fs::remove_dir_all(&stage).await;
                return Err(e.into());
            }
        }
        Ok(stage)
    }

    /// Selects the greatest semantic version among local releases for this slot.
    pub(crate) fn latest_component(&self, slot: Slot) -> Option<Arc<Release<Component>>> {
        let state = self.state();
        state
            .components
            .values()
            .filter(|r| r.slot() == slot)
            .filter_map(|r| Version::parse(r.version()).ok().map(|v| (v, r.id(), r)))
            .max_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)))
            .map(|(_, _, r)| r.clone())
    }

    async fn commit_component(
        &self,
        record: Arc<Release<Component>>,
        prepared: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Arc<Release<Component>>> {
        let id = record.id();
        let destination = record.directory(self.0.context.directories());
        record.validate(&destination)?;
        let _write = cancellation
            .run_until_cancelled(self.0.write.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut next = self.state().as_ref().clone();
        if let Some(current) = next.components.get(&id) {
            if current != &record {
                return Err(AddonError::InvalidRelease(destination).into());
            }
            current
                .require_payload(self.0.context.directories())
                .await?;
            return Ok(current.clone());
        }
        if next.contains(id) {
            return Err(AddonError::Duplicate(id).into());
        }
        if crate::utils::exists(&destination).await? {
            return Err(AddonError::TargetExists(destination).into());
        }
        next_config::save(prepared.join("release.toml"), record.as_ref()).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        async_fs::rename(prepared, destination).await?;
        next.components.insert(id, record.clone());
        self.publish(next);
        Ok(record)
    }

    async fn commit_dependency(
        &self,
        record: Arc<Release<Dependency>>,
        prepared: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Arc<Release<Dependency>>> {
        let id = record.id();
        let destination = record.directory(self.0.context.directories());
        record.validate(&destination)?;
        let _write = cancellation
            .run_until_cancelled(self.0.write.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut next = self.state().as_ref().clone();
        if let Some(current) = next.dependencies.get(&id) {
            if current != &record {
                return Err(AddonError::InvalidRelease(destination).into());
            }
            current
                .require_payload(self.0.context.directories())
                .await?;
            return Ok(current.clone());
        }
        if next.contains(id) {
            return Err(AddonError::Duplicate(id).into());
        }
        if crate::utils::exists(&destination).await? {
            return Err(AddonError::TargetExists(destination).into());
        }
        next_config::save(prepared.join("release.toml"), record.as_ref()).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        async_fs::rename(prepared, destination).await?;
        next.dependencies.insert(id, record.clone());
        self.publish(next);
        Ok(record)
    }

    fn state(&self) -> Arc<AddonsState> {
        self.0.published.borrow().clone()
    }

    /// Creates a unique staging directory on the same data tree as final storage.
    async fn create_stage(&self) -> Result<PathBuf> {
        let staging = self.0.context.directories().data_dir().join(".staging");
        async_fs::create_dir_all(&staging).await?;
        let stage = staging.join(Uuid::new_v4().to_string());
        async_fs::create_dir_all(&stage).await?;
        Ok(stage)
    }

    /// Publishes the already committed local snapshot without filesystem discovery.
    fn publish(&self, state: AddonsState) {
        self.0.published.send_replace(Arc::new(state));
    }
}

#[derive(Clone, Debug, Default)]
struct AddonsState {
    component_catalog: Option<Arc<Catalog<Component>>>,
    dependency_catalog: Option<Arc<Catalog<Dependency>>>,
    components: HashMap<Uuid, Arc<Release<Component>>>,
    dependencies: HashMap<Uuid, Arc<Release<Dependency>>>,
}
impl AddonsState {
    async fn load_cached(directories: &Directories) -> Result<Self> {
        let mut state = Self {
            component_catalog: Catalog::<Component>::load(directories).await,
            dependency_catalog: Catalog::<Dependency>::load(directories).await,
            ..Self::default()
        };
        for (id, path) in release_manifests(&directories.component_releases()).await? {
            let record: Release<Component> = next_config::load(&path).await?;
            record.validate(&path)?;
            if record.id() != id {
                return Err(AddonError::InvalidRelease(path).into());
            }
            if state.contains(id) {
                return Err(AddonError::Duplicate(id).into());
            }
            record.require_payload(directories).await?;
            state.components.insert(id, Arc::new(record));
        }
        for (id, path) in release_manifests(&directories.dependency_releases()).await? {
            let record: Release<Dependency> = next_config::load(&path).await?;
            record.validate(&path)?;
            if record.id() != id {
                return Err(AddonError::InvalidRelease(path).into());
            }
            if state.contains(id) {
                return Err(AddonError::Duplicate(id).into());
            }
            record.require_payload(directories).await?;
            state.dependencies.insert(id, Arc::new(record));
        }
        Ok(state)
    }

    fn contains(&self, id: Uuid) -> bool {
        self.components.contains_key(&id) || self.dependencies.contains_key(&id)
    }
}

// Component archives have one top-level directory, which becomes the payload.
async fn prepare_component_archive(
    archive: &Path,
    stage: &Path,
    cancellation: &CancellationToken,
) -> Result<PathBuf> {
    let extracted = stage.join("extracted");
    async_fs::create_dir(&extracted).await?;
    cancellation
        .run_until_cancelled(crate::utils::archive::extract(archive, &extracted))
        .await
        .ok_or(Error::Cancelled)??;
    let mut entries = async_fs::read_dir(&extracted).await?;
    let Some(entry) = entries.try_next().await? else {
        return Err(AddonError::InvalidComponentArchive.into());
    };
    if entries.try_next().await?.is_some() || !entry.file_type().await?.is_dir() {
        return Err(AddonError::InvalidComponentArchive.into());
    }
    let source = entry.path();
    check_component_links(&source, cancellation).await?;
    let prepared = stage.join("release");
    async_fs::create_dir(&prepared).await?;
    async_fs::rename(source, prepared.join("payload")).await?;
    Ok(prepared)
}

// Component links must stay inside the component tree after it leaves staging.
async fn check_component_links(root: &Path, cancellation: &CancellationToken) -> Result<()> {
    let root = async_fs::canonicalize(root).await?;
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        let mut entries = async_fs::read_dir(directory).await?;
        while let Some(entry) = entries.try_next().await? {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let path = entry.path();
            let kind = entry.file_type().await?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_symlink() {
                let target = async_fs::read_link(&path).await?;
                crate::utils::archive::safe_symlink_target(
                    path.strip_prefix(&root).unwrap(),
                    target,
                )?;
                if !async_fs::canonicalize(&path).await?.starts_with(&root) {
                    return Err(AddonError::InvalidComponent(path).into());
                }
            }
        }
    }
    Ok(())
}

async fn release_manifests(root: &Path) -> Result<Vec<(Uuid, PathBuf)>> {
    let mut manifests = Vec::new();
    let mut entries = async_fs::read_dir(root).await?;
    while let Some(entry) = entries.try_next().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        if let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) {
            manifests.push((id, entry.path().join("release.toml")));
        }
    }
    Ok(manifests)
}

/// Drives a download, translating its latest byte counts and cancellation result.
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
