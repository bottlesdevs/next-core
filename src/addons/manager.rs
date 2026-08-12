//! Shared addon catalogs, local discovery, downloads, and removal.

use std::{
    path::{Component as PathComponent, Path, PathBuf},
    sync::Arc,
};

use download_manager::{events::Progress as DownloadProgress, manager::DownloadManager};
use futures_core::Stream;
use futures_util::{FutureExt, StreamExt};
use semver::Version;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::{NonNilUuid, Uuid};

use super::{
    Addon, AddonError, CatalogError, Component, Dependency, Slot, Target,
    catalog::{Catalog, CatalogArtifact, CatalogEntry, CatalogKind, CatalogUrls},
    index::AddonIndex,
    item::Artifact,
};
use crate::{
    Directories, Operation, Progress, Stage, Transfer,
    error::{Error, Result},
    utils::{archive, checksum, exists},
};

/// A shared snapshot-based view of catalog and hand-placed addons.
///
/// Values returned by collection methods do not update after a refresh, fetch,
/// or removal. Query the manager again, or use [`watch`](Self::watch), to observe
/// a later publication.
#[derive(Clone)]
pub struct Addons(Arc<AddonsInner>);

struct AddonsInner {
    directories: Directories,
    catalog_urls: CatalogUrls,
    downloader: Arc<DownloadManager>,
    published: watch::Sender<Arc<AddonsState>>,
    /// Serializes filesystem commits and state publication, not transfers.
    write: Mutex<()>,
}

impl Addons {
    pub(crate) async fn load(
        directories: Directories,
        component_catalog_url: Option<Url>,
        dependency_catalog_url: Option<Url>,
        downloader: Arc<DownloadManager>,
    ) -> Result<Self> {
        let state = AddonsState::load_cached(&directories).await?;
        let (published, _) = watch::channel(Arc::new(state));
        Ok(Self(Arc::new(AddonsInner {
            directories,
            catalog_urls: CatalogUrls {
                components: component_catalog_url,
                dependencies: dependency_catalog_url,
            },
            downloader,
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
    pub fn components(&self) -> Vec<Arc<Addon<Component>>> {
        self.state().components.addons.values().cloned().collect()
    }

    /// Returns complete downloaded dependencies.
    pub fn dependencies(&self) -> Vec<Arc<Addon<Dependency>>> {
        self.state().dependencies.addons.values().cloned().collect()
    }

    /// Returns the local component with this immutable release identifier.
    pub fn component(&self, id: Uuid) -> Option<Arc<Addon<Component>>> {
        self.state().components.addons.get(&id).cloned()
    }

    /// Returns the local dependency with this immutable release identifier.
    pub fn dependency(&self, id: Uuid) -> Option<Arc<Addon<Dependency>>> {
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

    /// Refreshes the two configured catalogs independently.
    ///
    /// A successfully downloaded catalog is published even when the other
    /// catalog fails. In that case the operation returns [`CatalogError::Refresh`].
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
                    catalog.save(&addons.0.directories).await?;
                    Some(catalog.clone())
                }
                Err(_) => current.components.catalog.clone(),
            };
            let dependency_catalog = match &dependency {
                Ok(catalog) => {
                    catalog.save(&addons.0.directories).await?;
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

    /// Downloads, extracts, validates, and atomically publishes one component.
    pub fn fetch_component(
        &self,
        entry: &CatalogEntry<Component>,
    ) -> Operation<Arc<Addon<Component>>> {
        let addons = self.clone();
        let entry = entry.clone();
        Operation::new(move |progress, cancellation| async move {
            if let Some(component) = addons.component(entry.id()) {
                return Ok(component);
            }
            let target = Target::current().ok_or(CatalogError::Unsupported(entry.id()))?;
            let artifacts = entry.artifacts_for_target(target).collect::<Vec<_>>();
            if artifacts.is_empty() {
                return Err(CatalogError::Unsupported(entry.id()).into());
            }
            if artifacts.len() != 1 {
                return Err(CatalogError::InvalidComponentArtifactCount {
                    addon: entry.id(),
                    count: artifacts.len(),
                }
                .into());
            }
            let artifact = artifacts[0];
            if !single_path_component(entry.version())
                || !single_path_component(artifact.file_name())
            {
                return Err(CatalogError::InvalidEntry(entry.id()).into());
            }

            let stage = addons.create_stage().await?;
            let result = async {
                let file = stage.join(artifact.file_name());
                download_artifact(
                    &addons.0.downloader,
                    artifact,
                    &file,
                    progress,
                    &cancellation,
                )
                .await?;
                let extracted = stage.join("extracted");
                async_fs::create_dir_all(&extracted).await?;
                let extraction = archive::extract(&file, &extracted).fuse();
                let cancelled = cancellation.cancelled().fuse();
                futures_util::pin_mut!(extraction, cancelled);
                futures_util::select_biased! {
                    result = extraction => result?,
                    _ = cancelled => return Err(Error::Cancelled),
                }
                let release = top_level_directory(&extracted).await?;
                let slot = entry.slot();
                let requirements = AddonIndex::<Component>::inspect_release(slot, &release).await?;
                let _write = addons.0.write.lock().await;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                if let Some(component) = addons.component(entry.id()) {
                    return Ok(component);
                }
                let state = addons.state();
                let target =
                    AddonIndex::<Component>::target(&addons.0.directories, slot, entry.version())
                        .await?;
                if exists(&target).await? {
                    return Err(AddonError::TargetExists(target).into());
                }
                let component = Addon::new_component(
                    NonNilUuid::new(entry.id()).expect("catalog UUID is non-nil"),
                    entry.name().to_owned(),
                    entry.version().to_owned(),
                    slot,
                    requirements,
                    target.clone(),
                );
                let mut next = state.components.clone();
                next.addons.insert(component.id(), Arc::new(component));
                next.save(&addons.0.directories).await?;
                if let Err(error) = async_fs::rename(release, &target).await {
                    let _ = state.components.save(&addons.0.directories).await;
                    return Err(error.into());
                }
                let published = addons
                    .publish(
                        state.components.catalog.clone(),
                        state.dependencies.catalog.clone(),
                    )
                    .await
                    .and_then(|_| {
                        addons
                            .component(entry.id())
                            .ok_or_else(|| AddonError::NotFound(entry.id()).into())
                    });
                if published.is_err() {
                    let _ = async_fs::remove_dir_all(target).await;
                    let _ = state.components.save(&addons.0.directories).await;
                }
                published
            }
            .await;
            let _ = async_fs::remove_dir_all(stage).await;
            result
        })
    }

    /// Downloads every platform artifact and atomically publishes one dependency.
    pub fn fetch_dependency(
        &self,
        entry: &CatalogEntry<Dependency>,
    ) -> Operation<Arc<Addon<Dependency>>> {
        let addons = self.clone();
        let entry = entry.clone();
        Operation::new(move |progress, cancellation| async move {
            if let Some(dependency) = addons.dependency(entry.id()) {
                return Ok(dependency);
            }
            let target = Target::current().ok_or(CatalogError::Unsupported(entry.id()))?;
            let artifacts = entry.artifacts_for_target(target).collect::<Vec<_>>();
            if artifacts.is_empty() {
                return Err(CatalogError::Unsupported(entry.id()).into());
            }
            if artifacts
                .iter()
                .any(|artifact| !single_path_component(artifact.file_name()))
            {
                return Err(CatalogError::InvalidEntry(entry.id()).into());
            }

            let stage = addons.create_stage().await?;
            let result = async {
                for artifact in artifacts.iter().copied() {
                    download_artifact(
                        &addons.0.downloader,
                        artifact,
                        &stage.join(artifact.file_name()),
                        progress.clone(),
                        &cancellation,
                    )
                    .await?;
                }

                let _write = addons.0.write.lock().await;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                if let Some(dependency) = addons.dependency(entry.id()) {
                    return Ok(dependency);
                }
                let state = addons.state();
                let target =
                    AddonIndex::<Dependency>::target(&addons.0.directories, entry.id()).await?;
                if exists(&target).await? {
                    async_fs::remove_dir_all(&target).await?;
                }
                let dependency = Addon::new_dependency(
                    NonNilUuid::new(entry.id()).expect("catalog UUID is non-nil"),
                    entry.name().to_owned(),
                    entry.version().to_owned(),
                    entry.requirements().to_vec(),
                    target.clone(),
                    artifacts
                        .iter()
                        .map(|artifact| {
                            Artifact::new(
                                PathBuf::from(artifact.file_name()),
                                artifact.steps().to_vec(),
                            )
                        })
                        .collect(),
                );
                let mut next = state.dependencies.clone();
                next.addons.insert(dependency.id(), Arc::new(dependency));
                next.save(&addons.0.directories).await?;
                if let Err(error) = async_fs::rename(&stage, &target).await {
                    let _ = state.dependencies.save(&addons.0.directories).await;
                    return Err(error.into());
                }
                let published = addons
                    .publish(
                        state.components.catalog.clone(),
                        state.dependencies.catalog.clone(),
                    )
                    .await
                    .and_then(|_| {
                        addons
                            .dependency(entry.id())
                            .ok_or_else(|| AddonError::NotFound(entry.id()).into())
                    });
                if published.is_err() {
                    let _ = async_fs::remove_dir_all(target).await;
                    let _ = state.dependencies.save(&addons.0.directories).await;
                }
                published
            }
            .await;
            let _ = async_fs::remove_dir_all(stage).await;
            result
        })
    }

    /// Removes a component from shared storage without checking bottle references.
    pub async fn remove_component(&self, component: &Addon<Component>) -> Result<()> {
        let _write = self.0.write.lock().await;
        let state = self.state();
        let component = state
            .components
            .addons
            .get(&component.id())
            .ok_or(AddonError::NotFound(component.id()))?;
        state.components.remove_files(component).await?;
        let mut next = state.components.clone();
        next.addons.remove(&component.id());
        next.save(&self.0.directories).await?;
        self.publish(
            state.components.catalog.clone(),
            state.dependencies.catalog.clone(),
        )
        .await
    }

    /// Removes a dependency from shared storage without checking bottle references.
    pub async fn remove_dependency(&self, dependency: &Addon<Dependency>) -> Result<()> {
        let _write = self.0.write.lock().await;
        let state = self.state();
        let dependency = state
            .dependencies
            .addons
            .get(&dependency.id())
            .ok_or(AddonError::NotFound(dependency.id()))?;
        state.dependencies.remove_files(dependency).await?;
        let mut next = state.dependencies.clone();
        next.addons.remove(&dependency.id());
        next.save(&self.0.directories).await?;
        self.publish(
            state.components.catalog.clone(),
            state.dependencies.catalog.clone(),
        )
        .await
    }

    pub(crate) fn latest_component(&self, slot: Slot) -> Option<Arc<Addon<Component>>> {
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
        let staging = self.0.directories.data_dir().join(".staging");
        async_fs::create_dir_all(&staging).await?;
        let stage = staging.join(Uuid::new_v4().to_string());
        async_fs::create_dir_all(&stage).await?;
        Ok(stage)
    }

    async fn download_catalog<K>(
        &self,
        progress: watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<Arc<Catalog<K>>>
    where
        K: CatalogKind,
        Catalog<K>: DeserializeOwned,
    {
        let url = K::url(&self.0.catalog_urls).ok_or(CatalogError::UrlNotConfigured(K::LABEL))?;
        let staging = self.0.directories.data_dir().join(".staging");
        async_fs::create_dir_all(&staging).await?;
        let downloaded = staging.join(format!("catalog-{}.json", Uuid::new_v4()));
        let result = async {
            download(
                &self.0.downloader,
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
            let catalog =
                serde_json::from_slice::<Catalog<K>>(&async_fs::read(&downloaded).await?)?;
            Ok(Arc::new(catalog))
        }
        .await;
        let _ = async_fs::remove_file(downloaded).await;
        result
    }

    async fn publish(
        &self,
        component_catalog: Option<Arc<Catalog<Component>>>,
        dependency_catalog: Option<Arc<Catalog<Dependency>>>,
    ) -> Result<()> {
        let state =
            AddonsState::load(component_catalog, dependency_catalog, &self.0.directories).await?;
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

fn single_path_component(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(PathComponent::Normal(_))) && components.next().is_none()
}

async fn download_artifact(
    downloader: &DownloadManager,
    artifact: &CatalogArtifact,
    destination: &Path,
    progress: watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    download(
        downloader,
        artifact.url().clone(),
        destination,
        cancellation,
        |transfer| {
            progress.send_replace(Some(Progress::transferring(
                Stage::Downloading {
                    file: artifact.file_name().to_owned(),
                },
                transfer,
            )));
        },
    )
    .await?;
    progress.send_replace(Some(Progress::new(Stage::Verifying {
        file: artifact.file_name().to_owned(),
    })));
    if !checksum::verify(destination, artifact.checksum()).await? {
        return Err(AddonError::ChecksumMismatch(destination.to_path_buf()).into());
    }
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(())
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

async fn top_level_directory(root: &Path) -> Result<PathBuf> {
    let mut entries = async_fs::read_dir(root).await?;
    let Some(entry) = entries.next().await.transpose()? else {
        return Err(AddonError::InvalidComponentArchive.into());
    };
    if entries.next().await.transpose()?.is_some() || !entry.file_type().await?.is_dir() {
        return Err(AddonError::InvalidComponentArchive.into());
    }
    Ok(entry.path())
}
