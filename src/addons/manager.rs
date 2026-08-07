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
    error::{Error, Result},
    utils::{archive, exists},
};

#[derive(Clone)]
pub struct Addons(Arc<AddonsInner>);

struct AddonsInner {
    directories: Directories,
    component_catalog_url: Option<Url>,
    dependency_catalog_url: Option<Url>,
    downloader: Arc<DownloadManager>,
    published: watch::Sender<Arc<AddonsState>>,
    write: Mutex<()>,
}

impl Addons {
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

    pub fn runners(&self) -> Vec<RunnerComponent> {
        let mut runners = self.state().runners.values().cloned().collect::<Vec<_>>();
        runners.sort_unstable_by_key(RunnerComponent::id);
        runners
    }

    pub fn addons(&self) -> Vec<Addon> {
        let mut addons = self.state().addons.values().cloned().collect::<Vec<_>>();
        addons.sort_unstable_by_key(Addon::id);
        addons
    }

    pub fn watch(&self) -> impl Stream<Item = Self> + Send + 'static {
        let addons = self.clone();
        tokio_stream::StreamExt::map(WatchStream::new(self.0.published.subscribe()), move |_| {
            addons.clone()
        })
    }

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
            index::remove(&self.0.directories, id).await?;
        }
        self.publish(
            state.component_catalog.clone(),
            state.dependency_catalog.clone(),
        )
        .await
    }

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
            index::record(
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
pub enum CatalogKind {
    Components,
    Dependencies,
}

#[derive(Debug, Error)]
pub enum AddonError {
    #[error("catalog refresh failed (components: {components:?}, dependencies: {dependencies:?})")]
    CatalogRefresh {
        components: Option<String>,
        dependencies: Option<String>,
    },
    #[error("{0:?} catalog URL is not configured")]
    CatalogUrlNotConfigured(CatalogKind),
    #[error("item {item} does not belong in the {catalog:?} catalog")]
    WrongCatalog { item: Uuid, catalog: CatalogKind },
    #[error("catalogs contain duplicate item {0}")]
    DuplicateItem(Uuid),
    #[error("addon item {0} was not found")]
    ItemNotFound(Uuid),
    #[error("addon item {0} is not downloaded")]
    ItemNotDownloaded(Uuid),
    #[error("no artifact supports this system for item {0}")]
    UnsupportedItem(Uuid),
    #[error("internal item {0} is not downloaded")]
    InternalNotDownloaded(&'static str),
    #[error("checksum mismatch for {0}")]
    ChecksumMismatch(PathBuf),
    #[error("an extracted artifact must contain exactly one top-level directory")]
    InvalidArchive,
    #[error("component archive contains {found}, expected {expected}")]
    ComponentKindMismatch { expected: String, found: String },
    #[error("hand-placed component could not be identified: {0}")]
    InvalidHandPlacedComponent(PathBuf),
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
struct StoredItem {
    kind: ItemKind,
    path: PathBuf,
}

impl AddonsState {
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
                    index::record(
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
            let (downloader, _scheduler) = download_manager::manager::DownloadManager::new(
                Arc::new(client),
                download_manager::manager::DownloadManagerConfig::default(),
            );
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
            let (downloader, scheduler) =
                DownloadManager::new(client, DownloadManagerConfig::default());
            let scheduler = executor.spawn(scheduler);
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
            scheduler.await;
            fs::remove_dir_all(root).unwrap();
        }));
    }
}
