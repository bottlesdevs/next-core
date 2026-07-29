use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use download_manager::{
    download::DownloadResult, events::Progress as DownloadProgress, manager::DownloadManager,
};
use futures_core::Stream;
use futures_util::{FutureExt, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use super::{
    components::{
        Component,
        catalog::{CatalogComponentEntry, ComponentCatalog, ComponentKind},
        storage as component_storage,
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
};

#[derive(Clone)]
pub struct Library(Arc<LibraryInner>);

struct LibraryInner {
    directories: Directories,
    component_catalog_url: Url,
    dependency_catalog_url: Url,
    downloads: Arc<DownloadManager>,
    published: watch::Sender<Arc<LibraryState>>,
    write: Mutex<()>,
}

impl Library {
    pub(crate) async fn load(
        directories: Directories,
        component_catalog_url: Url,
        dependency_catalog_url: Url,
        downloads: Arc<DownloadManager>,
    ) -> Result<Self> {
        let component_catalog: Option<Arc<ComponentCatalog>> =
            load_cached_catalog(&component_catalog_path(&directories)).await;
        let dependency_catalog: Option<Arc<DependencyCatalog>> =
            load_cached_catalog(&dependency_catalog_path(&directories)).await;
        let components = component_storage::load(&directories).await?;
        let dependencies = dependency_storage::load(&directories).await?;
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
        let components = component_storage::load(&self.0.directories).await?;
        let dependencies = dependency_storage::load(&self.0.directories).await?;
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
            library.publish(
                component
                    .as_ref()
                    .ok()
                    .cloned()
                    .or_else(|| current.component_catalog.clone()),
                dependency
                    .as_ref()
                    .ok()
                    .cloned()
                    .or_else(|| current.dependency_catalog.clone()),
                current.downloaded_components(),
                current.downloaded_dependencies(),
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

    pub(crate) fn component_steps(&self, component: &Component) -> Vec<InstallStep> {
        self.state().component_steps(component)
    }

    async fn download_catalog(
        &self,
        catalog: CatalogKind,
        url: Url,
        progress: watch::Sender<Option<LibraryProgress>>,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf> {
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
        if result.is_err() {
            let _ = async_fs::remove_file(&destination).await;
        }
        result.map(|result| result.path)
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

#[derive(Clone, Debug)]
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
        components: Vec<Component>,
        dependencies: Vec<Dependency>,
    ) -> Self {
        let mut matched_components = HashSet::new();
        let mut component_statuses = component_catalog
            .iter()
            .flat_map(|catalog| catalog.as_ref())
            .map(|entry| {
                let downloaded = components
                    .iter()
                    .find(|component| component.id() == entry.uuid())
                    .cloned();
                if downloaded.is_some() {
                    matched_components.insert(entry.uuid());
                }
                ComponentStatus {
                    catalog: Some(entry.clone()),
                    downloaded,
                }
            })
            .collect::<Vec<_>>();
        component_statuses.extend(
            components
                .into_iter()
                .filter(|component| !matched_components.contains(&component.id()))
                .map(|downloaded| ComponentStatus {
                    catalog: None,
                    downloaded: Some(downloaded),
                }),
        );

        let mut matched_dependencies = HashSet::new();
        let mut dependency_statuses = dependency_catalog
            .iter()
            .flat_map(|catalog| catalog.as_ref())
            .map(|entry| {
                let downloaded = dependencies
                    .iter()
                    .find(|dependency| dependency.id() == entry.uuid())
                    .cloned();
                if downloaded.is_some() {
                    matched_dependencies.insert(entry.uuid());
                }
                DependencyStatus {
                    catalog: Some(entry.clone()),
                    downloaded,
                }
            })
            .collect::<Vec<_>>();
        dependency_statuses.extend(
            dependencies
                .into_iter()
                .filter(|dependency| !matched_dependencies.contains(&dependency.id()))
                .map(|downloaded| DependencyStatus {
                    catalog: None,
                    downloaded: Some(downloaded),
                }),
        );

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
            .component_catalog
            .as_deref()
            .and_then(|catalog| {
                catalog
                    .into_iter()
                    .find(|entry| entry.uuid() == component.id())
            })
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

    pub fn catalog(&self) -> Option<&CatalogComponentEntry> {
        self.catalog.as_ref()
    }

    pub fn downloaded(&self) -> Option<&Component> {
        self.downloaded.as_ref()
    }
}

#[derive(Clone, Debug)]
pub struct DependencyStatus {
    catalog: Option<CatalogDependencyEntry>,
    downloaded: Option<Dependency>,
}

impl DependencyStatus {
    pub fn id(&self) -> Uuid {
        self.catalog
            .as_ref()
            .map(CatalogDependencyEntry::uuid)
            .or_else(|| self.downloaded.as_ref().map(Dependency::id))
            .expect("dependency status has catalog metadata or a download")
    }

    pub fn name(&self) -> &str {
        self.catalog
            .as_ref()
            .map(CatalogDependencyEntry::name)
            .or_else(|| self.downloaded.as_ref().map(Dependency::name))
            .expect("dependency status has catalog metadata or a download")
    }

    pub fn version(&self) -> &str {
        self.catalog
            .as_ref()
            .map(CatalogDependencyEntry::version)
            .or_else(|| self.downloaded.as_ref().map(Dependency::version))
            .expect("dependency status has catalog metadata or a download")
    }

    pub fn catalog(&self) -> Option<&CatalogDependencyEntry> {
        self.catalog.as_ref()
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
) -> Result<DownloadResult> {
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
            result = result => return Ok(result?),
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
    T: DeserializeOwned + Serialize,
{
    let result = async {
        let bytes = async_fs::read(download).await?;
        let catalog = serde_json::from_slice(&bytes)?;
        let temporary = cache.with_extension("tmp");
        async_fs::write(&temporary, serde_json::to_vec_pretty(&catalog)?).await?;
        async_fs::rename(temporary, cache).await?;
        Ok(catalog)
    }
    .await;
    let _ = async_fs::remove_file(download).await;
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

    use super::*;

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
                .catalog()
                .is_some()
        );
        assert!(
            state
                .component(local_component.id())
                .unwrap()
                .catalog()
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
                Url::parse("https://example.test/components.json").unwrap(),
                Url::parse("https://example.test/dependencies.json").unwrap(),
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
}
