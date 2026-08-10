//! Shared addon state, downloads, publication, and managed-file removal.
//!
//! The manager reconciles cached catalogs, the component index, and filesystem
//! contents into immutable snapshots. Mutations rebuild and publish the complete
//! state; downloads may overlap, while filesystem commits and publication are
//! serialized.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use download_manager::{events::Progress as DownloadProgress, manager::DownloadManager};
use futures_core::Stream;
use futures_lite::io::AsyncReadExt;
use futures_util::{FutureExt, StreamExt};
use sha2::{Digest, Sha256, Sha512};
use thiserror::Error;
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::{NonNilUuid, Uuid};

use super::{
    Checksum, Target,
    catalog::{Catalog, CatalogArtifact, CatalogEntry, InternalRole, ItemKind, category},
    index,
    installer::recipe_steps,
    item::{Addon, InternalComponent, Resource, RunnerComponent},
};
use crate::{
    Directories, Operation, Progress, Stage, Transfer,
    addons::index::ComponentIndex,
    error::{Error, Result},
    utils::{archive, exists},
};

#[derive(Clone)]
/// A shared view of the available runners and bottle addons.
///
/// Clones are inexpensive handles to the same live state and may be used
/// concurrently. Collection methods return owned item values that do not update
/// after catalog refreshes, downloads, or removals. Query the collection again,
/// or use [`watch`](Self::watch) to observe state publications.
///
/// The manager uses services owned by the [`crate::Bottles`] instance that
/// created it. Using this handle after that instance has been closed is
/// unsupported.
///
/// Every publication rescans the supported local component tree. An invalid
/// entry in a recognized hand-placed category can therefore fail an otherwise
/// unrelated refresh, fetch, or removal while rebuilding state.
/// Concurrent mutations are safe, but their download phases may overlap and
/// callers that require ordering must await them in that order; final state is
/// determined by serialized commit order.
///
/// # Example
///
/// ```no_run
/// use bottles_core::{Availability, Bottles, Config};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let bottles = Bottles::open(Config {
///     component_catalog: Some("https://example.test/components.json".parse()?),
///     dependency_catalog: Some("https://example.test/dependencies.json".parse()?),
///     ..Config::default()
/// })
/// .await?;
/// let addons = bottles.addons();
/// addons.refresh().await?;
///
/// if let Some(addon) = addons
///     .addons()
///     .into_iter()
///     .find(|addon| addon.availability() == Availability::Downloadable)
/// {
///     let id = addon.id();
///     addons.fetch(id).await?;
///     let downloaded = addons.addons().into_iter().find(|addon| addon.id() == id);
///     assert_eq!(downloaded.unwrap().availability(), Availability::Downloaded);
/// }
///
/// bottles.close().await?;
/// # Ok(())
/// # }
/// ```
pub struct Addons(Arc<AddonsInner>);

struct AddonsInner {
    directories: Directories,
    component_catalog_url: Option<Url>,
    dependency_catalog_url: Option<Url>,
    downloader: Arc<DownloadManager>,
    published: watch::Sender<Arc<AddonsState>>,
    /// Serializes managed-file changes and publication; downloads run outside this lock.
    write: Mutex<()>,
}

impl Addons {
    /// Loads cached catalogs and scans the library-managed component directories.
    ///
    /// Missing, malformed, or invalid cached catalogs are ignored independently.
    /// Errors while scanning local components or rebuilding their index are
    /// returned to the caller.
    pub(crate) async fn load(
        directories: Directories,
        component_catalog_url: Option<Url>,
        dependency_catalog_url: Option<Url>,
        downloader: Arc<DownloadManager>,
    ) -> Result<Self> {
        let component_catalog = load_catalog(
            &catalog_path(&directories, CatalogKind::Components),
            CatalogKind::Components,
        )
        .await;
        let dependency_catalog = load_catalog(
            &catalog_path(&directories, CatalogKind::Dependencies),
            CatalogKind::Dependencies,
        )
        .await;
        let state = AddonsState::load(component_catalog, dependency_catalog, &directories).await?;
        let (published, _) = watch::channel(Arc::new(state));
        Ok(Self(Arc::new(AddonsInner {
            directories,
            component_catalog_url,
            dependency_catalog_url,
            downloader,
            published,
            write: Mutex::new(()),
        })))
    }

    /// Returns an owned snapshot of the currently known runners.
    ///
    /// The returned vector and its items are clones and do not update when the
    /// manager changes. Its ordering is unspecified; call this method again or
    /// use [`watch`](Self::watch) for newer state.
    pub fn runners(&self) -> Vec<RunnerComponent> {
        let mut runners = self.state().runners.values().cloned().collect::<Vec<_>>();
        runners.sort_unstable_by_key(RunnerComponent::id);
        runners
    }

    /// Returns an owned snapshot of the currently known bottle addons.
    ///
    /// The returned vector and its items are clones and do not update when the
    /// manager changes. Its ordering is unspecified; call this method again or
    /// use [`watch`](Self::watch) for newer state.
    pub fn addons(&self) -> Vec<Addon> {
        let mut addons = self.state().addons.values().cloned().collect::<Vec<_>>();
        addons.sort_unstable_by_key(Addon::id);
        addons
    }

    /// Watches the latest addon state.
    ///
    /// The stream first yields once for the current state, then once for observed
    /// publications. Slow consumers may see publications coalesced, and callers
    /// must tolerate notifications whose visible values equal a previous state.
    /// Each item is a live handle to this manager, not the state that caused the
    /// notification; call [`runners`](Self::runners) or [`addons`](Self::addons)
    /// to clone the latest values.
    ///
    /// The returned stream retains the manager until the stream is dropped and
    /// therefore does not end merely because the originating [`crate::Bottles`]
    /// value is dropped.
    pub fn watch(&self) -> impl Stream<Item = Self> + Send + 'static {
        let addons = self.clone();
        tokio_stream::StreamExt::map(WatchStream::new(self.0.published.subscribe()), move |_| {
            addons.clone()
        })
    }

    /// Downloads and publishes the configured component and dependency catalogs.
    ///
    /// Both catalogs are downloaded, decoded, and validated independently. If
    /// one of those phases fails, the successful catalog is persisted and
    /// published with the previous version of the failed catalog, provided
    /// persistence and state rebuilding succeed; the operation then returns
    /// [`AddonError::CatalogRefresh`]. If both fail, the existing catalogs are
    /// still rebuilt and published before that error is returned.
    ///
    /// Persistence and state rebuilding are not transactional. An I/O or rebuild
    /// failure may leave one catalog file updated on disk without publishing that
    /// state, and is returned directly instead of as `CatalogRefresh`.
    ///
    /// # Errors
    ///
    /// In addition to [`AddonError::CatalogRefresh`], the operation returns errors
    /// from writing catalog files or rebuilding addon state. Cancellation is
    /// checked after both download attempts and before waiting for the commit lock;
    /// persistence and publication after that check are not cancellable.
    pub fn refresh(&self) -> Operation<()> {
        let library = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let component = library
                .download_catalog(
                    CatalogKind::Components,
                    library.0.component_catalog_url.clone(),
                    progress.clone(),
                    &cancellation,
                )
                .await;
            let dependency = library
                .download_catalog(
                    CatalogKind::Dependencies,
                    library.0.dependency_catalog_url.clone(),
                    progress,
                    &cancellation,
                )
                .await;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }

            let _write = library.0.write.lock().await;
            let current = library.state();
            let component_catalog = match &component {
                Ok((catalog, bytes)) => {
                    async_fs::write(
                        catalog_path(&library.0.directories, CatalogKind::Components),
                        bytes,
                    )
                    .await?;
                    Some(catalog.clone())
                }
                Err(_) => current.component_catalog.clone(),
            };
            let dependency_catalog = match &dependency {
                Ok((catalog, bytes)) => {
                    async_fs::write(
                        catalog_path(&library.0.directories, CatalogKind::Dependencies),
                        bytes,
                    )
                    .await?;
                    Some(catalog.clone())
                }
                Err(_) => current.dependency_catalog.clone(),
            };
            library
                .publish(component_catalog, dependency_catalog)
                .await?;

            match (component, dependency) {
                (Ok(_), Ok(_)) => Ok(()),
                (component, dependency) => Err(AddonError::CatalogRefresh {
                    components: component.err().map(|error| error.to_string()),
                    dependencies: dependency.err().map(|error| error.to_string()),
                }
                .into()),
            }
        })
    }

    /// Ensures the item identified by `id` is downloaded for the current platform.
    ///
    /// An item already recorded as downloaded, including a hand-placed component,
    /// succeeds without doing any work or revalidating its path. Otherwise the item
    /// must be in a catalog.
    /// Every selected artifact is checksum-verified; component archives must extract
    /// to one top-level directory whose detected kind matches the catalog. A
    /// successful item is committed to library-managed storage and published.
    ///
    /// Concurrent fetches of the same UUID may perform duplicate transfers. They
    /// converge when committing: once one fetch publishes the item, another
    /// succeeds without replacing it. This is idempotency, not a guarantee that
    /// transfers are coalesced.
    ///
    /// Commit is not fully transactional. In particular, a dependency directory
    /// may remain in place if rebuilding state fails after its final rename, and a
    /// component index may have been rewritten before a later publication failure.
    ///
    /// # Errors
    ///
    /// The operation fails if the item is unknown, has no artifact for the current
    /// platform, cannot be downloaded, verified, extracted, or classified, or
    /// conflicts with an existing target on disk. Cancellation is observed while
    /// transferring, verifying, and extracting. When the operation remains polled
    /// through cancellation, staging files are then removed on a best-effort basis.
    /// Dropping a started operation abandons it and may leave staging files because
    /// it drops the cleanup future. Cancellation does not roll back a commit already
    /// in progress.
    // TODO: Return Operation<Addon>
    pub fn fetch(&self, id: Uuid) -> Operation<()> {
        let library = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let state = library.state();
            if state.is_downloaded(id) {
                return Ok(());
            }
            let entry = state
                .entry(id)
                .cloned()
                .ok_or(AddonError::ItemNotFound(id))?;
            let target = Target::current().ok_or(AddonError::UnsupportedItem(id))?;
            let artifacts = entry.matching_artifacts(target).collect::<Vec<_>>();
            if artifacts.is_empty() {
                return Err(AddonError::UnsupportedItem(id).into());
            }

            let staging_root = library.0.directories.data_dir().join(".staging");
            async_fs::create_dir_all(&staging_root).await?;
            let stage = staging_root.join(Uuid::new_v4().to_string());
            async_fs::create_dir_all(&stage).await?;
            let result = library
                .download_entry(&entry, artifacts, &stage, progress, &cancellation)
                .await;
            let _ = async_fs::remove_dir_all(&stage).await;
            result
        })
    }

    /// Deletes the downloaded files for an addon, runner, or internal component.
    ///
    /// This recursively removes the recorded path, including hand-placed
    /// components, without checking whether a bottle still refers to it. It does
    /// not undo an addon's changes inside any bottle; use
    /// [`crate::Bottle::uninstall`] for that. Removing a referenced runner or
    /// component can therefore make existing bottles unusable.
    ///
    /// Existing [`Addon`] and [`RunnerComponent`] snapshots are not updated.
    /// Removal is not transactional: deletion is not rolled back if a later index
    /// update or state publication fails.
    /// Unlike [`refresh`](Self::refresh) and [`fetch`](Self::fetch), removal does
    /// not return an [`Operation`] and provides no progress or cancellation handle.
    ///
    /// # Errors
    ///
    /// Returns [`AddonError::ItemNotFound`] if the UUID is not recorded as
    /// downloaded or its recorded path no longer exists. Filesystem, index, and
    /// state-rebuild failures are returned directly.
    pub async fn remove(&self, id: Uuid) -> Result<()> {
        let _write = self.0.write.lock().await;
        let state = self.state();
        let stored = state
            .stored
            .get(&id)
            .cloned()
            .ok_or(AddonError::ItemNotFound(id))?;
        if !exists(&stored.path).await? {
            return Err(AddonError::ItemNotFound(id).into());
        }
        async_fs::remove_dir_all(&stored.path).await?;
        if stored.kind.is_single_artifact() {
            ComponentIndex::remove(&self.0.directories, id).await?;
        }
        self.publish(
            state.component_catalog.clone(),
            state.dependency_catalog.clone(),
        )
        .await
    }

    /// Reads the latest WineBridge selection without downloading or provisioning one.
    ///
    /// # Errors
    ///
    /// Returns [`AddonError::InternalNotDownloaded`] when no WineBridge directory
    /// was recorded in the latest state.
    pub(crate) fn winebridge(&self) -> Result<InternalComponent> {
        self.state()
            .internals
            .get(&InternalRole::Winebridge)
            .cloned()
            .ok_or_else(|| AddonError::InternalNotDownloaded("winebridge").into())
    }

    fn state(&self) -> Arc<AddonsState> {
        self.0.published.borrow().clone()
    }

    /// Uses a temporary file that is removed best-effort on both success and
    /// returned failure. Dropping the owning operation can drop this cleanup future.
    /// The returned bytes are the exact payload to persist after parsing.
    // TODO: Dont return raw bytes. Re-serialize the Catalog when needed
    async fn download_catalog(
        &self,
        kind: CatalogKind,
        url: Option<Url>,
        progress: watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<(Arc<Catalog>, Vec<u8>)> {
        let url = url.ok_or(AddonError::CatalogUrlNotConfigured(kind))?;
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
                    let (index, file) = match kind {
                        CatalogKind::Components => (1, "components catalog"),
                        CatalogKind::Dependencies => (2, "dependencies catalog"),
                    };
                    progress.send_replace(Some(Progress::transferring(
                        Stage::Downloading {
                            file: file.into(),
                            index,
                            total: 2,
                        },
                        transfer,
                    )));
                },
            )
            .await?;
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let bytes = async_fs::read(&downloaded).await?;
            let catalog = Arc::new(serde_json::from_slice::<Catalog>(&bytes)?);
            validate_catalog(&catalog, kind)?;
            Ok((catalog, bytes))
        }
        .await;
        let _ = async_fs::remove_file(downloaded).await;
        result
    }

    async fn download_entry(
        &self,
        entry: &CatalogEntry,
        artifacts: Vec<(usize, &CatalogArtifact)>,
        stage: &Path,
        progress: watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        if entry.kind().is_single_artifact() {
            self.download_component_entry(entry, &artifacts, stage, progress, cancellation)
                .await
        } else {
            self.download_dependency_entry(entry, &artifacts, stage, progress, cancellation)
                .await
        }
    }

    /// Only the first matching artifact is used. The archive must have one
    /// top-level directory and its contents must match the declared component
    /// kind. Commit and publication are serialized; if index recording or
    /// publication fails after the rename, the target is removed best-effort.
    // TODO: Maybe we can return `Addon`
    async fn download_component_entry(
        &self,
        entry: &CatalogEntry,
        artifacts: &[(usize, &CatalogArtifact)],
        stage: &Path,
        progress: watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let (_, artifact) = artifacts
            .first()
            .ok_or(AddonError::UnsupportedItem(entry.id()))?;
        let file = stage.join(artifact.file_name());
        download(
            &self.0.downloader,
            artifact.url().clone(),
            &file,
            cancellation,
            |transfer| {
                progress.send_replace(Some(Progress::transferring(
                    Stage::Downloading {
                        file: artifact.file_name().to_owned(),
                        index: 1,
                        total: 1,
                    },
                    transfer,
                )));
            },
        )
        .await?;
        progress.send_replace(Some(Progress::new(Stage::Verifying {
            file: artifact.file_name().to_owned(),
        })));
        verify_checksum(&file, artifact.checksum(), cancellation).await?;
        progress.send_replace(Some(Progress::new(Stage::Extracting)));
        let extracted = stage.join("extracted");
        async_fs::create_dir_all(&extracted).await?;
        {
            let extraction = archive::extract(&file, &extracted).fuse();
            let cancelled = cancellation.cancelled().fuse();
            futures_util::pin_mut!(extraction, cancelled);
            futures_util::select_biased! {
                result = extraction => result?,
                _ = cancelled => return Err(Error::Cancelled),
            }
        }
        let release = top_level_directory(&extracted).await?;
        let component_category = category(entry.kind()).expect("component-class item has category");
        let found = index::detect_kind(component_category, &release)
            .await?
            .ok_or_else(|| AddonError::InvalidHandPlacedComponent(release.clone()))?;
        if found != entry.kind() {
            return Err(AddonError::ComponentKindMismatch {
                expected: format!("{:?}", entry.kind()),
                found: format!("{found:?}"),
            }
            .into());
        }

        let category_root = self
            .0
            .directories
            .component_category(entry.kind())
            .expect("component-class item has category");
        let target = category_root.join(artifact.file_name());
        let _write = self.0.write.lock().await;
        if self.state().is_downloaded(entry.id()) {
            return Ok(());
        }
        if exists(&target).await? {
            return Err(AddonError::TargetExists(target).into());
        }
        async_fs::create_dir_all(category_root).await?;
        async_fs::rename(release, &target).await?;
        let result = async {
            ComponentIndex::record(
                &self.0.directories,
                entry.id(),
                entry.version().to_owned(),
                target.clone(),
                entry.kind(),
            )
            .await?;
            let state = self.state();
            self.publish(
                state.component_catalog.clone(),
                state.dependency_catalog.clone(),
            )
            .await
        }
        .await;
        if result.is_err() {
            let _ = async_fs::remove_dir_all(target).await;
        }
        result
    }

    /// Dependency artifacts remain as files and are moved into one directory named
    /// for the entry UUID. Commit and publication are serialized, but a successful
    /// rename is not rolled back if rebuilding state fails.
    // TODO: Maybe we can return `Addon`
    async fn download_dependency_entry(
        &self,
        entry: &CatalogEntry,
        artifacts: &[(usize, &CatalogArtifact)],
        stage: &Path,
        progress: watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let total = artifacts.len();
        for (resource, (_, artifact)) in artifacts.iter().enumerate() {
            let file = stage.join(artifact.file_name());
            download(
                &self.0.downloader,
                artifact.url().clone(),
                &file,
                cancellation,
                |transfer| {
                    progress.send_replace(Some(Progress::transferring(
                        Stage::Downloading {
                            file: artifact.file_name().to_owned(),
                            index: resource + 1,
                            total,
                        },
                        transfer,
                    )));
                },
            )
            .await?;
            progress.send_replace(Some(Progress::new(Stage::Verifying {
                file: artifact.file_name().to_owned(),
            })));
            verify_checksum(&file, artifact.checksum(), cancellation).await?;
        }

        let target = self.0.directories.dependency(entry.id());
        let _write = self.0.write.lock().await;
        if self.state().is_downloaded(entry.id()) {
            let _ = async_fs::remove_dir_all(stage).await;
            return Ok(());
        }
        if exists(&target).await? {
            return Err(AddonError::TargetExists(target).into());
        }
        async_fs::rename(stage, &target).await?;
        let state = self.state();
        self.publish(
            state.component_catalog.clone(),
            state.dependency_catalog.clone(),
        )
        .await
    }

    async fn publish(
        &self,
        component_catalog: Option<Arc<Catalog>>,
        dependency_catalog: Option<Arc<Catalog>>,
    ) -> Result<()> {
        let state =
            AddonsState::load(component_catalog, dependency_catalog, &self.0.directories).await?;
        self.0.published.send_replace(Arc::new(state));
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Catalog placement determines which item classes are accepted during refresh.
pub enum CatalogKind {
    /// Runners, internal components, and slot-based addons.
    Components,
    /// Addons that are installed as dependencies without occupying a slot.
    Dependencies,
}

#[derive(Debug, Error)]
/// Errors raised while loading, refreshing, downloading, or removing addons.
///
/// Lower-level I/O, decoding, download, archive, runner, and cancellation failures
/// are represented by other variants of [`crate::error::Error`].
pub enum AddonError {
    /// One or both catalogs failed to refresh.
    ///
    /// A `None` field means that catalog refreshed successfully. Successful
    /// catalogs have already been published when this error is returned.
    #[error("catalog refresh failed (components: {components:?}, dependencies: {dependencies:?})")]
    CatalogRefresh {
        components: Option<String>,
        dependencies: Option<String>,
    },
    /// No download URL was configured for the named catalog during refresh.
    ///
    /// The public refresh operation normally stores this error's text in the
    /// corresponding [`Self::CatalogRefresh`] field.
    #[error("{0:?} catalog URL is not configured")]
    CatalogUrlNotConfigured(CatalogKind),
    /// A catalog contains an item class that is only valid in the other catalog.
    ///
    /// For a downloaded catalog, the public refresh operation normally stores this
    /// error's text in the corresponding [`Self::CatalogRefresh`] field.
    #[error("item {item} does not belong in the {catalog:?} catalog")]
    WrongCatalog { item: Uuid, catalog: CatalogKind },
    /// Multiple catalog or local-index items use the same UUID.
    #[error("catalogs contain duplicate item {0}")]
    DuplicateItem(Uuid),
    /// The requested UUID cannot be fetched or removed.
    ///
    /// Fetching produces this error for UUIDs absent from both catalogs. Removal
    /// also produces it when the item is known but is not recorded as downloaded,
    /// or when its recorded path is missing.
    #[error("addon item {0} was not found")]
    ItemNotFound(Uuid),
    /// A runner or addon snapshot has no recorded downloaded resources.
    #[error("addon item {0} is not downloaded")]
    ItemNotDownloaded(Uuid),
    /// The item has no artifact compatible with the current platform, or the
    /// current platform cannot be represented by [`Target`].
    #[error("no artifact supports this system for item {0}")]
    UnsupportedItem(Uuid),
    /// A library-managed support component required for the operation is missing.
    ///
    /// The contained value is the component role used in the error message. The
    /// operation does not provision the component automatically.
    #[error("internal item {0} is not downloaded")]
    InternalNotDownloaded(&'static str),
    /// A downloaded artifact's exact lowercase digest did not match its catalog
    /// checksum.
    #[error("checksum mismatch for {0}")]
    ChecksumMismatch(PathBuf),
    /// A component archive was empty or did not contain exactly one top-level
    /// directory and no other top-level entries.
    #[error("an extracted artifact must contain exactly one top-level directory")]
    InvalidArchive,
    /// A downloaded component's detected kind differs from its catalog entry.
    #[error("component archive contains {found}, expected {expected}")]
    ComponentKindMismatch { expected: String, found: String },
    /// A component directory could not be represented as a supported local item.
    ///
    /// This covers unclassifiable extracted or hand-placed components and
    /// hand-placed no-slot dependencies, which are not supported by local scanning.
    #[error("hand-placed component could not be identified: {0}")]
    InvalidHandPlacedComponent(PathBuf),
    /// Committing a download would overwrite an existing target not recorded as
    /// this item.
    #[error("addon target already exists: {0}")]
    TargetExists(PathBuf),
}

#[derive(Clone, Debug)]
struct AddonsState {
    component_catalog: Option<Arc<Catalog>>,
    dependency_catalog: Option<Arc<Catalog>>,
    runners: HashMap<Uuid, RunnerComponent>,
    addons: HashMap<Uuid, Addon>,
    internals: HashMap<InternalRole, InternalComponent>,
    stored: HashMap<Uuid, StoredItem>,
}

#[derive(Clone, Debug)]
/// Recorded on-disk ownership needed to make fetch idempotent and remove items.
struct StoredItem {
    /// Component-class items also require index removal; dependencies do not.
    kind: ItemKind,
    /// Root path recursively removed by [`Addons::remove`].
    path: PathBuf,
}

impl AddonsState {
    /// Catalog entries are processed before distinct hand-placed components.
    /// Catalog-backed components refresh their local index record. When several
    /// downloaded internal entries have the same role, catalog order selects the
    /// first. If no catalog entry is downloaded, the first path-sorted hand-placed
    /// component is used. All Proton runners are paired with the selected UMU
    /// component, if present. This routine only pairs existing files and does not
    /// provision UMU.
    async fn load(
        component_catalog: Option<Arc<Catalog>>,
        dependency_catalog: Option<Arc<Catalog>>,
        directories: &Directories,
    ) -> Result<Self> {
        let mut state = Self {
            component_catalog: component_catalog.clone(),
            dependency_catalog: dependency_catalog.clone(),
            runners: HashMap::new(),
            addons: HashMap::new(),
            internals: HashMap::new(),
            stored: HashMap::new(),
        };
        let discovered = index::scan(directories).await?;
        let target = Target::current();
        let mut ids = std::collections::HashSet::new();
        let mut catalog_component_paths = std::collections::HashSet::new();
        for entry in component_catalog
            .iter()
            .chain(dependency_catalog.iter())
            .flat_map(|catalog| catalog.items())
        {
            if !ids.insert(entry.id()) {
                return Err(AddonError::DuplicateItem(entry.id()).into());
            }
            let matching = target
                .map(|target| entry.matching_artifacts(target).collect::<Vec<_>>())
                .unwrap_or_default();
            let supported = !matching.is_empty();
            let (resources, stored_path) = catalog_resources(entry, &matching, directories).await;
            if let Some(path) = stored_path {
                if entry.kind().is_single_artifact() {
                    catalog_component_paths.insert(path.clone());
                    ComponentIndex::record(
                        directories,
                        entry.id(),
                        entry.version().to_owned(),
                        path.clone(),
                        entry.kind(),
                    )
                    .await?;
                }
                state.stored.insert(
                    entry.id(),
                    StoredItem {
                        kind: entry.kind(),
                        path,
                    },
                );
            } else if entry.kind().is_single_artifact()
                && let Some((_, artifact)) = matching.first()
            {
                catalog_component_paths.insert(
                    directories
                        .component_category(entry.kind())
                        .expect("component-class item has category")
                        .join(artifact.file_name()),
                );
            }
            let id = NonNilUuid::new(entry.id()).expect("catalog UUID is non-nil");
            match entry.kind() {
                ItemKind::RunnerComponent { flavour } => {
                    state.runners.insert(
                        entry.id(),
                        RunnerComponent::new(
                            id,
                            entry.name().to_owned(),
                            entry.version().to_owned(),
                            flavour,
                            resources
                                .first()
                                .map(|resource| resource.source().to_path_buf()),
                            supported,
                        ),
                    );
                }
                ItemKind::Addon { slot } => {
                    state.addons.insert(
                        entry.id(),
                        Addon::new(
                            id,
                            entry.name().to_owned(),
                            entry.version().to_owned(),
                            slot,
                            resources,
                            supported,
                        ),
                    );
                }
                ItemKind::InternalComponent { role } => {
                    if let Some(resource) = resources.first() {
                        state.internals.entry(role).or_insert_with(|| {
                            InternalComponent::new(id, role, resource.source().to_path_buf())
                        });
                    }
                }
            }
        }

        for component in discovered {
            if catalog_component_paths.contains(component.path()) {
                continue;
            }
            if !ids.insert(component.id()) {
                return Err(AddonError::DuplicateItem(component.id()).into());
            }
            let id = NonNilUuid::new(component.id()).expect("index UUID is non-nil");
            let version = component.version().to_owned();
            let path = component.path().to_path_buf();
            match component.kind() {
                ItemKind::RunnerComponent { flavour } => {
                    state.runners.insert(
                        component.id(),
                        RunnerComponent::new(
                            id,
                            version.clone(),
                            version,
                            flavour,
                            Some(path.clone()),
                            true,
                        ),
                    );
                }
                ItemKind::Addon { slot: Some(slot) } => {
                    state.addons.insert(
                        component.id(),
                        Addon::new(
                            id,
                            version.clone(),
                            version,
                            Some(slot),
                            vec![Resource::new(path.clone(), recipe_steps(slot).to_vec())],
                            true,
                        ),
                    );
                }
                ItemKind::InternalComponent { role } => {
                    state
                        .internals
                        .entry(role)
                        .or_insert_with(|| InternalComponent::new(id, role, path.clone()));
                }
                ItemKind::Addon { slot: None } => {
                    return Err(AddonError::InvalidHandPlacedComponent(path).into());
                }
            }
            state.stored.insert(
                component.id(),
                StoredItem {
                    kind: component.kind(),
                    path,
                },
            );
        }
        let umu = state.internals.get(&InternalRole::Umu).cloned();
        for runner in state.runners.values_mut() {
            runner.pair_umu(umu.clone());
        }
        Ok(state)
    }

    fn entry(&self, id: Uuid) -> Option<&CatalogEntry> {
        self.component_catalog
            .as_ref()
            .and_then(|catalog| catalog.item(id))
            .or_else(|| {
                self.dependency_catalog
                    .as_ref()
                    .and_then(|catalog| catalog.item(id))
            })
    }

    fn is_downloaded(&self, id: Uuid) -> bool {
        self.stored.contains_key(&id)
    }
}

/// Component-class items require their single artifact path to be a directory.
/// Slot addons with no catalog recipe receive the built-in recipe for their slot.
/// Dependencies require every matching artifact to be a regular file; one missing
/// artifact makes the entire entry not downloaded. The returned root is the path
/// owned by the entry and recursively removed by [`Addons::remove`].
async fn catalog_resources(
    entry: &CatalogEntry,
    matching: &[(usize, &CatalogArtifact)],
    directories: &Directories,
) -> (Vec<Resource>, Option<PathBuf>) {
    if matching.is_empty() {
        return (Vec::new(), None);
    }
    if entry.kind().is_single_artifact() {
        let artifact = matching[0].1;
        let path = directories
            .component_category(entry.kind())
            .expect("component-class item has category")
            .join(artifact.file_name());
        if !async_fs::metadata(&path)
            .await
            .is_ok_and(|metadata| metadata.is_dir())
        {
            return (Vec::new(), None);
        }
        let steps = match entry.kind() {
            ItemKind::Addon { slot: Some(slot) } if artifact.steps().is_empty() => {
                recipe_steps(slot).to_vec()
            }
            _ => artifact.steps().to_vec(),
        };
        return (vec![Resource::new(path.clone(), steps)], Some(path));
    }

    let root = directories.dependency(entry.id());
    let mut resources = Vec::with_capacity(matching.len());
    for (_, artifact) in matching {
        let source = root.join(artifact.file_name());
        if !async_fs::metadata(&source)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return (Vec::new(), None);
        }
        resources.push(Resource::new(source, artifact.steps().to_vec()));
    }
    (resources, Some(root))
}

fn validate_catalog(catalog: &Catalog, kind: CatalogKind) -> Result<()> {
    for item in catalog.items() {
        let valid = matches!(
            (kind, item.kind()),
            (
                CatalogKind::Components,
                ItemKind::RunnerComponent { .. }
                    | ItemKind::InternalComponent { .. }
                    | ItemKind::Addon { slot: Some(_) },
            ) | (CatalogKind::Dependencies, ItemKind::Addon { slot: None })
        );
        if !valid {
            return Err(AddonError::WrongCatalog {
                item: item.id(),
                catalog: kind,
            }
            .into());
        }
    }
    Ok(())
}

/// Loads a cached catalog, treating any read, decode, or validation failure as absent.
///
/// Startup deliberately ignores an unusable catalog so local items and the other
/// independently cached catalog remain available.
async fn load_catalog(path: &Path, kind: CatalogKind) -> Option<Arc<Catalog>> {
    let bytes = async_fs::read(path).await.ok()?;
    let catalog = Arc::new(serde_json::from_slice(&bytes).ok()?);
    validate_catalog(&catalog, kind).ok()?;
    Some(catalog)
}

fn catalog_path(directories: &Directories, kind: CatalogKind) -> PathBuf {
    match kind {
        CatalogKind::Components => directories.components().join("catalog.json"),
        CatalogKind::Dependencies => directories.dependencies().join("catalog.json"),
    }
}

/// Downloads one URL while forwarding byte progress and observing cancellation.
///
/// Cancellation asks the download manager to cancel the transfer and waits for
/// that request before returning [`Error::Cancelled`]. The destination may contain
/// a partial file; its caller owns cleanup.
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

/// The file is read in fixed-size chunks so memory use is constant. Cancellation
/// is checked between chunks. Comparison is exact against lowercase hexadecimal
/// output.
async fn verify_checksum(
    path: &Path,
    checksum: &Checksum,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut file = async_fs::File::open(path).await?;
    let mut buffer = vec![0; 64 * 1024];
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    loop {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        match checksum {
            Checksum::Sha256(_) => sha256.update(&buffer[..read]),
            Checksum::Sha512(_) => sha512.update(&buffer[..read]),
        }
    }
    let actual = match checksum {
        Checksum::Sha256(_) => format!("{:x}", sha256.finalize()),
        Checksum::Sha512(_) => format!("{:x}", sha512.finalize()),
    };
    if actual != checksum.value() {
        return Err(AddonError::ChecksumMismatch(path.to_path_buf()).into());
    }
    Ok(())
}

/// Component archives must contain exactly one top-level directory.
///
/// Empty archives, multiple entries, or a sole non-directory entry produce
/// [`AddonError::InvalidArchive`].
async fn top_level_directory(root: &Path) -> Result<PathBuf> {
    let mut entries = async_fs::read_dir(root).await?;
    let Some(entry) = entries.next().await.transpose()? else {
        return Err(AddonError::InvalidArchive.into());
    };
    if entries.next().await.transpose()?.is_some() || !entry.file_type().await?.is_dir() {
        return Err(AddonError::InvalidArchive.into());
    }
    Ok(entry.path())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use download_manager::manager::DownloadManagerConfig;
    use http::Response;
    use http_client::{MockClient, body};
    use smol_tar::{TarRegularFile, TarWriter};

    use super::*;

    async fn tar(path: &str, contents: &'static [u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut archive = TarWriter::new(&mut bytes);
            archive
                .write(
                    TarRegularFile::new(path, contents.len() as u64, contents)
                        .with_mode(0o755)
                        .into(),
                )
                .await
                .unwrap();
            archive.finish().await.unwrap();
        }
        bytes
    }

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn catalog(id: &str, kind: &str, file_name: &str) -> Catalog {
        serde_json::from_str(&format!(
            r#"{{
                "schema_version": 1,
                "items": [{{
                    "id": "{id}",
                    "name": "test",
                    "version": "1",
                    "kind": {kind},
                    "artifacts": [{{
                        "url": "https://example.test/item",
                        "file_name": "{file_name}",
                        "checksum": {{ "algorithm": "sha256", "value": "abc" }}
                    }}]
                }}]
            }}"#,
        ))
        .unwrap()
    }

    #[test]
    fn catalogs_accept_only_their_addon_class() {
        let component = catalog(
            "00000000-0000-0000-0000-000000000001",
            r#"{ "type": "addon", "slot": "dxvk" }"#,
            "dxvk",
        );
        let dependency = catalog(
            "00000000-0000-0000-0000-000000000002",
            r#"{ "type": "addon" }"#,
            "dependency.dll",
        );

        validate_catalog(&component, CatalogKind::Components).unwrap();
        validate_catalog(&dependency, CatalogKind::Dependencies).unwrap();
        assert!(validate_catalog(&component, CatalogKind::Dependencies).is_err());
        assert!(validate_catalog(&dependency, CatalogKind::Components).is_err());
    }

    #[test]
    fn catalog_items_use_component_and_dependency_layouts() {
        futures_lite::future::block_on(async {
            let root = std::env::temp_dir().join(format!("bottles-next-layout-{}", Uuid::new_v4()));
            let directories = Directories::from_path(&root).unwrap();
            let component_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
            let dependency_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
            let component_path = directories.components().join("dxvk/dxvk-2.4");
            let dependency_path = directories.dependency(dependency_id);
            fs::create_dir_all(&component_path).unwrap();
            fs::create_dir_all(&dependency_path).unwrap();
            fs::write(dependency_path.join("dependency.dll"), []).unwrap();

            let state = AddonsState::load(
                Some(Arc::new(catalog(
                    &component_id.to_string(),
                    r#"{ "type": "addon", "slot": "dxvk" }"#,
                    "dxvk-2.4",
                ))),
                Some(Arc::new(catalog(
                    &dependency_id.to_string(),
                    r#"{ "type": "addon" }"#,
                    "dependency.dll",
                ))),
                &directories,
            )
            .await
            .unwrap();

            assert_eq!(state.stored[&component_id].path, component_path);
            assert_eq!(state.stored[&dependency_id].path, dependency_path);
            assert!(
                index::scan(&directories)
                    .await
                    .unwrap()
                    .iter()
                    .any(|component| component.id() == component_id)
            );
            fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    fn hand_placed_addon_uses_recipe_and_can_be_removed() {
        futures_lite::future::block_on(async {
            let root =
                std::env::temp_dir().join(format!("bottles-next-library-{}", Uuid::new_v4()));
            let directories = Directories::from_path(&root).unwrap();
            let path = directories.components().join("dxvk/2.4");
            fs::create_dir_all(&path).unwrap();
            let client =
                http_client::MockClient::new(|_| Ok(http::Response::new(http_client::body([]))));
            let downloader = download_manager::manager::DownloadManager::new(
                Arc::new(client),
                download_manager::manager::DownloadManagerConfig::default(),
            )
            .unwrap();
            let library = Addons::load(directories.clone(), None, None, Arc::new(downloader))
                .await
                .unwrap();

            let addon = library.addons().pop().unwrap();
            assert_eq!(addon.slot(), Some(super::super::Slot::Dxvk));
            assert_eq!(addon.availability(), super::super::Availability::Downloaded);
            assert!(!addon.prepare().unwrap()[0].steps.is_empty());
            library.remove(addon.id()).await.unwrap();
            assert!(library.addons().is_empty());
            assert!(!path.exists());
            fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    fn fetch_and_remove_use_restored_layouts() {
        let executor = async_executor::Executor::new();
        futures_lite::future::block_on(executor.run(async {
            let component_id = Uuid::new_v4();
            let dependency_id = Uuid::new_v4();
            let component_archive = tar("dxvk-1/x64/d3d11.dll", b"dll").await;
            let dependency_file = b"installer".to_vec();
            let root = std::env::temp_dir().join(format!("bottles-next-fetch-{}", Uuid::new_v4()));
            let directories = Directories::from_path(&root).unwrap();
            async_fs::write(
                catalog_path(&directories, CatalogKind::Components),
                serde_json::json!({
                    "schema_version": 1,
                    "items": [{
                        "id": component_id,
                        "name": "DXVK",
                        "version": "1",
                        "kind": { "type": "addon", "slot": "dxvk" },
                        "artifacts": [{
                            "url": "https://example.test/dxvk.tar",
                            "file_name": "dxvk.tar",
                            "checksum": {
                                "algorithm": "sha256",
                                "value": sha256(&component_archive)
                            }
                        }]
                    }]
                })
                .to_string(),
            )
            .await
            .unwrap();
            async_fs::write(
                catalog_path(&directories, CatalogKind::Dependencies),
                serde_json::json!({
                    "schema_version": 1,
                    "items": [{
                        "id": dependency_id,
                        "name": "Runtime",
                        "version": "1",
                        "kind": { "type": "addon" },
                        "artifacts": [{
                            "url": "https://example.test/runtime.exe",
                            "file_name": "runtime.exe",
                            "checksum": {
                                "algorithm": "sha256",
                                "value": sha256(&dependency_file)
                            }
                        }]
                    }]
                })
                .to_string(),
            )
            .await
            .unwrap();
            let component_body = component_archive.clone();
            let dependency_body = dependency_file.clone();
            let client = Arc::new(MockClient::new(move |request| {
                let bytes = if request.uri().path().ends_with("dxvk.tar") {
                    component_body.clone()
                } else {
                    dependency_body.clone()
                };
                Ok(Response::builder().status(200).body(body(bytes))?)
            }));
            let downloader =
                DownloadManager::new(client, DownloadManagerConfig::default()).unwrap();
            let library = Addons::load(directories.clone(), None, None, Arc::new(downloader))
                .await
                .unwrap();

            library.fetch(component_id).await.unwrap();
            library.fetch(dependency_id).await.unwrap();
            assert!(directories.components().join("dxvk/dxvk.tar").is_dir());
            assert!(
                directories
                    .dependency(dependency_id)
                    .join("runtime.exe")
                    .is_file()
            );
            assert!(
                index::scan(&directories)
                    .await
                    .unwrap()
                    .iter()
                    .any(|component| component.id() == component_id)
            );

            library.remove(component_id).await.unwrap();
            library.remove(dependency_id).await.unwrap();
            assert!(!directories.components().join("dxvk/dxvk.tar").exists());
            assert!(!directories.dependency(dependency_id).exists());
            drop(library);
            fs::remove_dir_all(root).unwrap();
        }));
    }
}
