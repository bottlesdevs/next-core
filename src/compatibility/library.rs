use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use download_manager::{events::Progress as DownloadProgress, manager::DownloadManager};
use futures_core::Stream;
use futures_lite::io::AsyncReadExt;
use futures_util::{FutureExt, StreamExt};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256, Sha512};
use thiserror::Error;
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use super::{
    Checksum, Target,
    components::{
        Component,
        catalog::{CatalogComponentEntry, ComponentCatalog, ComponentKind},
        index as component_index,
    },
    dependencies::{
        Dependency,
        catalog::{CatalogDependencyEntry, DependencyCatalog},
        storage as dependency_storage,
    },
    installer::{InstallStep, component_steps},
};
use crate::{
    Directories, Operation, Transfer,
    error::{Error, Result},
    utils::{archive, exists},
};

#[derive(Clone)]
pub struct Library(Arc<LibraryInner>);

struct LibraryInner {
    directories: Directories,
    component_catalog_url: Option<Url>,
    dependency_catalog_url: Option<Url>,
    downloads: Arc<DownloadManager>,
    published: watch::Sender<Arc<LibraryState>>,
    write: Mutex<()>,
}

impl Library {
    pub(crate) async fn load(
        directories: Directories,
        component_catalog_url: Option<Url>,
        dependency_catalog_url: Option<Url>,
        downloads: Arc<DownloadManager>,
    ) -> Result<Self> {
        let component_catalog: Option<Arc<ComponentCatalog>> =
            load_cached_catalog(&component_catalog_path(&directories)).await;
        let dependency_catalog: Option<Arc<DependencyCatalog>> =
            load_cached_catalog(&dependency_catalog_path(&directories)).await;
        let components = component_index::scan(&directories).await?;
        let dependencies =
            dependency_storage::scan(&directories, dependency_catalog.as_deref()).await?;
        let (published, _) = watch::channel(Arc::new(LibraryState::new(
            component_catalog,
            dependency_catalog,
            components,
            dependencies,
        )));
        Ok(Self(Arc::new(LibraryInner {
            directories,
            component_catalog_url,
            dependency_catalog_url,
            downloads,
            published,
            write: Mutex::new(()),
        })))
    }

    pub fn state(&self) -> Arc<LibraryState> {
        self.0.published.borrow().clone()
    }

    pub fn updates(&self) -> impl Stream<Item = Arc<LibraryState>> + Send + 'static {
        WatchStream::new(self.0.published.subscribe())
    }

    pub async fn refresh(&self) -> Result<()> {
        let _write = self.0.write.lock().await;
        let current = self.state();
        let components = component_index::scan(&self.0.directories).await?;
        let dependencies =
            dependency_storage::scan(&self.0.directories, current.dependency_catalog.as_deref())
                .await?;
        self.publish(
            current.component_catalog.clone(),
            current.dependency_catalog.clone(),
            components,
            dependencies,
        );
        Ok(())
    }

    pub fn refresh_catalogs(&self) -> Operation<(), LibraryProgress> {
        let library = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let component = library.download_catalog(
                CatalogKind::Components,
                library.0.component_catalog_url.clone(),
                progress.clone(),
                &cancellation,
            );
            let dependency = library.download_catalog(
                CatalogKind::Dependencies,
                library.0.dependency_catalog_url.clone(),
                progress.clone(),
                &cancellation,
            );
            let (component, dependency) = futures_util::future::join(component, dependency).await;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }

            let component = match component {
                Ok(path) => {
                    progress.send_replace(Some(LibraryProgress::LoadingCatalog(
                        CatalogKind::Components,
                    )));
                    save_downloaded_catalog::<ComponentCatalog>(
                        &path,
                        &component_catalog_path(&library.0.directories),
                    )
                    .await
                    .map(Arc::new)
                }
                Err(error) => Err(error),
            };
            let dependency = match dependency {
                Ok(path) => {
                    progress.send_replace(Some(LibraryProgress::LoadingCatalog(
                        CatalogKind::Dependencies,
                    )));
                    save_downloaded_catalog::<DependencyCatalog>(
                        &path,
                        &dependency_catalog_path(&library.0.directories),
                    )
                    .await
                    .map(Arc::new)
                }
                Err(error) => Err(error),
            };

            let _write = library.0.write.lock().await;
            let current = library.state();
            let component_catalog = component
                .as_ref()
                .ok()
                .cloned()
                .or_else(|| current.component_catalog.clone());
            let dependency_catalog = dependency
                .as_ref()
                .ok()
                .cloned()
                .or_else(|| current.dependency_catalog.clone());
            let dependencies =
                dependency_storage::scan(&library.0.directories, dependency_catalog.as_deref())
                    .await?;
            library.publish(
                component_catalog,
                dependency_catalog,
                current.downloaded_components(),
                dependencies,
            );

            match (component, dependency) {
                (Ok(_), Ok(_)) => Ok(()),
                (component, dependency) => Err(LibraryError::CatalogRefresh {
                    components: component.err().map(|error| error.to_string()),
                    dependencies: dependency.err().map(|error| error.to_string()),
                }
                .into()),
            }
        })
    }

    pub fn download_component(&self, id: Uuid) -> Operation<Component, LibraryProgress> {
        let library = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let state = library.state();
            let status = state
                .component(id)
                .ok_or(LibraryError::ComponentNotFound(id))?;
            if let Some(component) = status.downloaded() {
                return Ok(component.clone());
            }
            let entry = status
                .catalog
                .clone()
                .ok_or(LibraryError::ComponentNotFound(id))?;
            let target = Target::current().ok_or(LibraryError::UnsupportedComponent(id))?;
            let artifact = entry
                .artifact_for(target)
                .cloned()
                .ok_or(LibraryError::UnsupportedComponent(id))?;
            let stage = library
                .0
                .directories
                .components()
                .join(".staging")
                .join(Uuid::new_v4().to_string());
            async_fs::create_dir_all(&stage).await?;

            let result = library
                .download_component_entry(entry, artifact, &stage, progress, &cancellation)
                .await;
            let _ = async_fs::remove_dir_all(stage).await;
            result
        })
    }

    pub async fn delete_component(&self, id: Uuid) -> Result<()> {
        let _write = self.0.write.lock().await;
        let state = self.state();
        let component = state
            .component(id)
            .and_then(ComponentStatus::downloaded)
            .cloned()
            .ok_or(LibraryError::ComponentNotFound(id))?;
        async_fs::remove_dir_all(component.path()).await?;
        let components = component_index::scan(&self.0.directories).await?;
        self.publish(
            state.component_catalog.clone(),
            state.dependency_catalog.clone(),
            components,
            state.downloaded_dependencies(),
        );
        Ok(())
    }

    pub fn download_dependency(&self, id: Uuid) -> Operation<Dependency, LibraryProgress> {
        let library = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let state = library.state();
            let status = state
                .dependency(id)
                .ok_or(LibraryError::DependencyNotFound(id))?;
            if let Some(dependency) = status.downloaded() {
                return Ok(dependency.clone());
            }
            let dependency = Dependency::from(&status.catalog);
            let stage = library
                .0
                .directories
                .dependencies()
                .join(".staging")
                .join(Uuid::new_v4().to_string());
            async_fs::create_dir_all(&stage).await?;

            let result = library
                .download_dependency_entry(dependency, &stage, progress, &cancellation)
                .await;
            let _ = async_fs::remove_dir_all(&stage).await;
            result
        })
    }

    pub async fn delete_dependency(&self, id: Uuid) -> Result<()> {
        let _write = self.0.write.lock().await;
        let state = self.state();
        state
            .dependency(id)
            .and_then(DependencyStatus::downloaded)
            .ok_or(LibraryError::DependencyNotFound(id))?;
        async_fs::remove_dir_all(self.0.directories.dependency(id)).await?;
        let dependencies =
            dependency_storage::scan(&self.0.directories, state.dependency_catalog.as_deref())
                .await?;
        self.publish(
            state.component_catalog.clone(),
            state.dependency_catalog.clone(),
            state.downloaded_components(),
            dependencies,
        );
        Ok(())
    }

    pub(crate) fn component_steps(&self, component: &Component) -> Vec<InstallStep> {
        self.state().component_steps(component)
    }

    async fn download_component_entry(
        &self,
        entry: CatalogComponentEntry,
        artifact: super::components::catalog::ComponentArtifact,
        stage: &Path,
        progress: watch::Sender<Option<LibraryProgress>>,
        cancellation: &CancellationToken,
    ) -> Result<Component> {
        let file_name = artifact.file_name();
        let archive_path = stage.join(file_name);
        download(
            &self.0.downloads,
            artifact.url().clone(),
            &archive_path,
            cancellation,
            |transfer| {
                progress.send_replace(Some(LibraryProgress::Downloading {
                    file: file_name.to_owned(),
                    resource: 1,
                    resources: 1,
                    transfer,
                }));
            },
        )
        .await?;
        progress.send_replace(Some(LibraryProgress::Verifying {
            file: file_name.to_owned(),
            resource: 1,
            resources: 1,
        }));
        verify_checksum(&archive_path, artifact.checksum(), cancellation).await?;

        progress.send_replace(Some(LibraryProgress::Extracting));
        let extracted = stage.join("extracted");
        async_fs::create_dir_all(&extracted).await?;
        let extraction = archive::extract(&archive_path, &extracted).fuse();
        let cancelled = cancellation.cancelled().fuse();
        futures_util::pin_mut!(extraction, cancelled);
        futures_util::select_biased! {
            result = extraction => result?,
            _ = cancelled => return Err(Error::Cancelled),
        }
        let release = top_level_directory(&extracted).await?;
        let category = entry.kind().directory_name();
        let found = component_index::detect_kind(category, &release)
            .await?
            .ok_or(LibraryError::InvalidComponentArchive)?;
        if found != entry.kind() {
            return Err(LibraryError::ComponentKindMismatch {
                expected: entry.kind(),
                found,
            }
            .into());
        }

        let category_root = self.0.directories.component_category(entry.kind());
        let target = category_root.join(artifact.file_name());
        let _write = self.0.write.lock().await;
        if let Some(component) = self
            .state()
            .component(entry.uuid())
            .and_then(ComponentStatus::downloaded)
        {
            return Ok(component.clone());
        }
        if exists(&target).await? {
            return Err(LibraryError::TargetExists(target).into());
        }
        async_fs::create_dir_all(category_root).await?;
        async_fs::rename(&release, &target).await?;

        let result = async {
            let component = Component::from_catalog_entry(&entry, &self.0.directories).await?;
            component_index::ComponentIndex::record(&self.0.directories, component.clone()).await?;
            let state = self.state();
            let mut components = state.downloaded_components();
            components.push(component.clone());
            self.publish(
                state.component_catalog.clone(),
                state.dependency_catalog.clone(),
                components,
                state.downloaded_dependencies(),
            );
            Ok(component)
        }
        .await;
        if result.is_err() {
            let _ = async_fs::remove_dir_all(target).await;
        }
        result
    }

    async fn download_dependency_entry(
        &self,
        dependency: Dependency,
        stage: &Path,
        progress: watch::Sender<Option<LibraryProgress>>,
        cancellation: &CancellationToken,
    ) -> Result<Dependency> {
        let id = dependency.id();
        let total = dependency.resources.len();
        for (index, resource) in dependency.resources.iter().enumerate() {
            let file_name = resource.file_name();
            let destination = stage.join(file_name);
            download(
                &self.0.downloads,
                resource.url().clone(),
                &destination,
                cancellation,
                |transfer| {
                    progress.send_replace(Some(LibraryProgress::Downloading {
                        file: file_name.to_owned(),
                        resource: index + 1,
                        resources: total,
                        transfer,
                    }));
                },
            )
            .await?;
            progress.send_replace(Some(LibraryProgress::Verifying {
                file: file_name.to_owned(),
                resource: index + 1,
                resources: total,
            }));
            verify_checksum(&destination, resource.checksum(), cancellation).await?;
        }

        let target = self.0.directories.dependency(id);
        let _write = self.0.write.lock().await;
        if let Some(dependency) = self
            .state()
            .dependency(id)
            .and_then(DependencyStatus::downloaded)
        {
            return Ok(dependency.clone());
        }
        if exists(&target).await? {
            return Err(LibraryError::TargetExists(target).into());
        }
        async_fs::rename(stage, &target).await?;

        let state = self.state();
        let mut dependencies = state.downloaded_dependencies();
        dependencies.push(dependency.clone());
        self.publish(
            state.component_catalog.clone(),
            state.dependency_catalog.clone(),
            state.downloaded_components(),
            dependencies,
        );
        Ok(dependency)
    }

    async fn download_catalog(
        &self,
        catalog: CatalogKind,
        url: Option<Url>,
        progress: watch::Sender<Option<LibraryProgress>>,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf> {
        let url = url.ok_or(LibraryError::CatalogUrlNotConfigured(catalog))?;
        let staging = self.0.directories.data_dir().join(".staging");
        async_fs::create_dir_all(&staging).await?;
        let destination = staging.join(format!("catalog-{}.json", Uuid::new_v4()));
        let result = download(
            &self.0.downloads,
            url,
            &destination,
            cancellation,
            |transfer| {
                progress.send_replace(Some(LibraryProgress::CatalogDownload { catalog, transfer }));
            },
        )
        .await;
        result?;
        Ok(destination)
    }

    fn publish(
        &self,
        component_catalog: Option<Arc<ComponentCatalog>>,
        dependency_catalog: Option<Arc<DependencyCatalog>>,
        components: Vec<Component>,
        dependencies: Vec<Dependency>,
    ) {
        self.0.published.send_replace(Arc::new(LibraryState::new(
            component_catalog,
            dependency_catalog,
            components,
            dependencies,
        )));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogKind {
    Components,
    Dependencies,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibraryProgress {
    CatalogDownload {
        catalog: CatalogKind,
        transfer: Transfer,
    },
    LoadingCatalog(CatalogKind),
    Downloading {
        file: String,
        resource: usize,
        resources: usize,
        transfer: Transfer,
    },
    Verifying {
        file: String,
        resource: usize,
        resources: usize,
    },
    Extracting,
}

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("catalog refresh failed (components: {components:?}, dependencies: {dependencies:?})")]
    CatalogRefresh {
        components: Option<String>,
        dependencies: Option<String>,
    },
    #[error("{0:?} catalog URL is not configured")]
    CatalogUrlNotConfigured(CatalogKind),
    #[error("component {0} was not found")]
    ComponentNotFound(Uuid),
    #[error("dependency {0} was not found")]
    DependencyNotFound(Uuid),
    #[error("no artifact supports this system for component {0}")]
    UnsupportedComponent(Uuid),
    #[error("checksum mismatch for {0}")]
    ChecksumMismatch(PathBuf),
    #[error("component archive must contain exactly one top-level directory")]
    InvalidComponentArchive,
    #[error("component archive contains {found:?}, expected {expected:?}")]
    ComponentKindMismatch {
        expected: ComponentKind,
        found: ComponentKind,
    },
    #[error("library target already exists: {0}")]
    TargetExists(PathBuf),
}

#[derive(Clone, Debug)]
pub struct LibraryState {
    components: Vec<ComponentStatus>,
    dependencies: Vec<DependencyStatus>,
    component_catalog: Option<Arc<ComponentCatalog>>,
    dependency_catalog: Option<Arc<DependencyCatalog>>,
}

impl LibraryState {
    fn new(
        component_catalog: Option<Arc<ComponentCatalog>>,
        dependency_catalog: Option<Arc<DependencyCatalog>>,
        mut components: Vec<Component>,
        dependencies: Vec<Dependency>,
    ) -> Self {
        let mut component_statuses = component_catalog
            .iter()
            .flat_map(|catalog| catalog.as_ref())
            .map(|entry| {
                let downloaded = components
                    .iter()
                    .position(|component| component.id() == entry.uuid())
                    .map(|index| components.remove(index));
                ComponentStatus {
                    catalog: Some(entry.clone()),
                    downloaded,
                }
            })
            .collect::<Vec<_>>();
        component_statuses.extend(components.into_iter().map(|downloaded| ComponentStatus {
            catalog: None,
            downloaded: Some(downloaded),
        }));

        let dependency_statuses = dependency_catalog
            .iter()
            .flat_map(|catalog| catalog.as_ref())
            .map(|entry| {
                let downloaded = dependencies
                    .iter()
                    .find(|dependency| dependency.id() == entry.uuid())
                    .cloned();
                DependencyStatus {
                    catalog: entry.clone(),
                    downloaded,
                }
            })
            .collect::<Vec<_>>();

        Self {
            components: component_statuses,
            dependencies: dependency_statuses,
            component_catalog,
            dependency_catalog,
        }
    }

    pub fn components(&self) -> &[ComponentStatus] {
        &self.components
    }

    pub fn component(&self, id: Uuid) -> Option<&ComponentStatus> {
        self.components
            .iter()
            .find(|component| component.id() == id)
    }

    pub fn dependencies(&self) -> &[DependencyStatus] {
        &self.dependencies
    }

    pub fn dependency(&self, id: Uuid) -> Option<&DependencyStatus> {
        self.dependencies
            .iter()
            .find(|dependency| dependency.id() == id)
    }

    pub fn has_component_catalog(&self) -> bool {
        self.component_catalog.is_some()
    }

    pub fn has_dependency_catalog(&self) -> bool {
        self.dependency_catalog.is_some()
    }

    fn downloaded_components(&self) -> Vec<Component> {
        self.components
            .iter()
            .filter_map(|status| status.downloaded.clone())
            .collect()
    }

    fn downloaded_dependencies(&self) -> Vec<Dependency> {
        self.dependencies
            .iter()
            .filter_map(|status| status.downloaded.clone())
            .collect()
    }

    fn component_steps(&self, component: &Component) -> Vec<InstallStep> {
        if let Some(steps) = self
            .component(component.id())
            .and_then(|status| status.catalog.as_ref())
            .map(CatalogComponentEntry::steps)
            .filter(|steps| !steps.is_empty())
        {
            return steps.to_vec();
        }

        component_steps(component.kind())
            .unwrap_or_default()
            .to_vec()
    }
}

#[derive(Clone, Debug)]
pub struct ComponentStatus {
    catalog: Option<CatalogComponentEntry>,
    downloaded: Option<Component>,
}

impl ComponentStatus {
    pub fn id(&self) -> Uuid {
        self.catalog
            .as_ref()
            .map(CatalogComponentEntry::uuid)
            .or_else(|| self.downloaded.as_ref().map(Component::id))
            .expect("component status has catalog metadata or a download")
    }

    pub fn version(&self) -> &str {
        self.catalog
            .as_ref()
            .map(CatalogComponentEntry::version)
            .or_else(|| self.downloaded.as_ref().map(Component::version))
            .expect("component status has catalog metadata or a download")
    }

    pub fn kind(&self) -> ComponentKind {
        self.catalog
            .as_ref()
            .map(CatalogComponentEntry::kind)
            .or_else(|| self.downloaded.as_ref().map(Component::kind))
            .expect("component status has catalog metadata or a download")
    }

    pub fn downloaded(&self) -> Option<&Component> {
        self.downloaded.as_ref()
    }
}

#[derive(Clone, Debug)]
pub struct DependencyStatus {
    catalog: CatalogDependencyEntry,
    downloaded: Option<Dependency>,
}

impl DependencyStatus {
    pub fn id(&self) -> Uuid {
        self.catalog.uuid()
    }

    pub fn name(&self) -> &str {
        self.catalog.name()
    }

    pub fn version(&self) -> &str {
        self.catalog.version()
    }

    pub fn downloaded(&self) -> Option<&Dependency> {
        self.downloaded.as_ref()
    }
}

async fn download(
    downloads: &DownloadManager,
    url: Url,
    destination: &Path,
    cancellation: &CancellationToken,
    mut on_progress: impl FnMut(Transfer),
) -> Result<()> {
    let download = downloads.download(url, destination)?;
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
    let actual = match checksum {
        Checksum::Sha256(_) => digest::<Sha256>(path, cancellation).await?,
        Checksum::Sha512(_) => digest::<Sha512>(path, cancellation).await?,
    };
    if actual.eq_ignore_ascii_case(checksum.value()) {
        Ok(())
    } else {
        Err(LibraryError::ChecksumMismatch(path.to_path_buf()).into())
    }
}

async fn digest<D>(path: &Path, cancellation: &CancellationToken) -> Result<String>
where
    D: Digest + Default,
    sha2::digest::Output<D>: std::fmt::LowerHex,
{
    let mut file = async_fs::File::open(path).await?;
    let mut digest = D::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

async fn top_level_directory(root: &Path) -> Result<PathBuf> {
    let mut entries = async_fs::read_dir(root).await?;
    let Some(entry) = entries.next().await.transpose()? else {
        return Err(LibraryError::InvalidComponentArchive.into());
    };
    if entries.next().await.transpose()?.is_some() || !entry.file_type().await?.is_dir() {
        return Err(LibraryError::InvalidComponentArchive.into());
    }
    Ok(entry.path())
}

async fn load_cached_catalog<T>(path: &Path) -> Option<Arc<T>>
where
    T: DeserializeOwned,
{
    match async_fs::read(path).await {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(catalog) => Some(Arc::new(catalog)),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "ignoring invalid catalog cache");
                None
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "ignoring unreadable catalog cache");
            None
        }
    }
}

async fn save_downloaded_catalog<T>(download: &Path, cache: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let result = async {
        let bytes = async_fs::read(download).await?;
        let catalog = serde_json::from_slice(&bytes)?;
        async_fs::rename(download, cache).await?;
        Ok(catalog)
    }
    .await;
    if result.is_err() {
        let _ = async_fs::remove_file(download).await;
    }
    result
}

fn component_catalog_path(directories: &Directories) -> PathBuf {
    directories.components().join("catalog.json")
}

fn dependency_catalog_path(directories: &Directories) -> PathBuf {
    directories.dependencies().join("catalog.json")
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn state_joins_catalog_downloads_and_resolves_recipes() {
        let catalog_component = Component::new(
            ComponentKind::Dxvk,
            "1",
            std::env::temp_dir().join("catalog-dxvk"),
        )
        .unwrap();
        let local_component = Component::new(
            ComponentKind::Vkd3d,
            "local",
            std::env::temp_dir().join("local-vkd3d"),
        )
        .unwrap();
        let catalog: ComponentCatalog = serde_json::from_str(&format!(
            r#"{{
                "schema_version": 1,
                "items": [{{
                    "id": "{}",
                    "version": "1",
                    "kind": {{ "type": "dxvk" }},
                    "artifacts": [{{
                        "url": "https://example.test/dxvk.tar.gz",
                        "file_name": "dxvk.tar.gz",
                        "checksum": {{ "algorithm": "sha256", "value": "abc" }}
                    }}],
                    "steps": [{{
                        "action": "set-environment",
                        "name": "FROM_CATALOG",
                        "value": "yes"
                    }}]
                }}]
            }}"#,
            catalog_component.id()
        ))
        .unwrap();
        let state = LibraryState::new(
            Some(Arc::new(catalog)),
            None,
            vec![catalog_component.clone(), local_component.clone()],
            Vec::new(),
        );

        assert_eq!(state.components().len(), 2);
        assert!(
            state
                .component(catalog_component.id())
                .unwrap()
                .catalog
                .is_some()
        );
        assert!(
            state
                .component(local_component.id())
                .unwrap()
                .catalog
                .is_none()
        );
        assert!(matches!(
            state.component_steps(&catalog_component).as_slice(),
            [InstallStep::SetEnvironment { name, value }]
                if name == "FROM_CATALOG" && value == "yes"
        ));
        assert!(!state.component_steps(&local_component).is_empty());
    }

    #[test]
    fn catalog_refresh_publishes_independent_successes() {
        let executor = async_executor::Executor::new();
        futures_lite::future::block_on(executor.run(async {
            let component_catalog = r#"{
                "schema_version": 1,
                "items": [{
                    "id": "00000000-0000-0000-0000-000000000002",
                    "version": "1",
                    "kind": { "type": "dxvk" },
                    "artifacts": [{
                        "url": "https://example.test/dxvk.tar.gz",
                        "file_name": "dxvk.tar.gz",
                        "checksum": { "algorithm": "sha256", "value": "abc" }
                    }]
                }]
            }"#;
            let client = Arc::new(MockClient::new(move |request| {
                let bytes = if request.uri().path().ends_with("components.json") {
                    component_catalog
                } else {
                    "invalid dependency catalog"
                };
                Ok(Response::builder().status(200).body(body(bytes))?)
            }));
            let (downloads, scheduler) =
                DownloadManager::new(client, DownloadManagerConfig::default());
            let scheduler = executor.spawn(scheduler);
            let root =
                std::env::temp_dir().join(format!("bottles-next-library-{}", Uuid::new_v4()));
            let directories = Directories::from_path(&root).unwrap();
            let library = Library::load(
                directories,
                Some(Url::parse("https://example.test/components.json").unwrap()),
                Some(Url::parse("https://example.test/dependencies.json").unwrap()),
                Arc::new(downloads),
            )
            .await
            .unwrap();

            assert!(library.refresh_catalogs().await.is_err());
            assert!(library.state().has_component_catalog());
            assert!(!library.state().has_dependency_catalog());

            drop(library);
            scheduler.await;
            async_fs::remove_dir_all(root).await.unwrap();
        }));
    }

    #[test]
    fn downloads_and_deletes_components_and_dependencies() {
        let executor = async_executor::Executor::new();
        futures_lite::future::block_on(executor.run(async {
            let component_id = Uuid::new_v4();
            let dependency_id = Uuid::new_v4();
            let component_archive = tar("dxvk-1/x64/d3d11.dll", b"dll").await;
            let dependency_file = b"installer".to_vec();
            let root =
                std::env::temp_dir().join(format!("bottles-next-library-{}", Uuid::new_v4()));
            let directories = Directories::from_path(&root).unwrap();
            async_fs::write(
                component_catalog_path(&directories),
                format!(
                    r#"{{
                        "schema_version": 1,
                        "items": [{{
                            "id": "{component_id}",
                            "version": "1",
                            "kind": {{ "type": "dxvk" }},
                            "artifacts": [{{
                                "url": "https://example.test/dxvk.tar",
                                "file_name": "dxvk.tar",
                                "checksum": {{
                                    "algorithm": "sha256",
                                    "value": "{}"
                                }}
                            }}]
                        }}]
                    }}"#,
                    sha256(&component_archive)
                ),
            )
            .await
            .unwrap();
            async_fs::write(
                dependency_catalog_path(&directories),
                format!(
                    r#"{{
                        "schema_version": 1,
                        "items": [{{
                            "id": "{dependency_id}",
                            "name": "runtime",
                            "version": "1",
                            "resources": [{{
                                "url": "https://example.test/runtime.exe",
                                "file_name": "runtime.exe",
                                "checksum": {{
                                    "algorithm": "sha256",
                                    "value": "{}"
                                }},
                                "target_arch": "x86_64",
                                "steps": []
                            }}]
                        }}]
                    }}"#,
                    sha256(&dependency_file)
                ),
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
            let (downloads, scheduler) =
                DownloadManager::new(client, DownloadManagerConfig::default());
            let scheduler = executor.spawn(scheduler);
            let library = Library::load(directories, None, None, Arc::new(downloads))
                .await
                .unwrap();

            let component = library.download_component(component_id).await.unwrap();
            assert_eq!(component.id(), component_id);
            assert!(component.path().ends_with("dxvk/dxvk.tar"));
            let dependency = library.download_dependency(dependency_id).await.unwrap();
            assert_eq!(dependency.id(), dependency_id);
            assert!(
                root.join(format!("dependencies/{dependency_id}/runtime.exe"))
                    .is_file()
            );

            library.delete_component(component_id).await.unwrap();
            library.delete_dependency(dependency_id).await.unwrap();
            assert!(
                library
                    .state()
                    .component(component_id)
                    .unwrap()
                    .downloaded()
                    .is_none()
            );
            assert!(
                library
                    .state()
                    .dependency(dependency_id)
                    .unwrap()
                    .downloaded()
                    .is_none()
            );

            drop(library);
            scheduler.await;
            async_fs::remove_dir_all(root).await.unwrap();
        }));
    }
}
