use std::{collections::HashSet, sync::Arc};

use download_manager::manager::DownloadManager;
use futures_core::Stream;
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
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
use crate::{Directories, error::Result};

pub struct Library {
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
    ) -> Result<Arc<Self>> {
        let components = component_storage::load(&directories).await?;
        let dependencies = dependency_storage::load(&directories).await?;
        let (published, _) = watch::channel(Arc::new(LibraryState::new(
            None,
            None,
            components,
            dependencies,
        )));
        Ok(Arc::new(Self {
            directories,
            component_catalog_url,
            dependency_catalog_url,
            downloads,
            published,
            write: Mutex::new(()),
        }))
    }

    pub fn state(&self) -> Arc<LibraryState> {
        self.published.borrow().clone()
    }

    pub fn updates(&self) -> impl Stream<Item = Arc<LibraryState>> + Send + 'static {
        WatchStream::new(self.published.subscribe())
    }

    pub async fn refresh(&self) -> Result<()> {
        let _write = self.write.lock().await;
        let current = self.state();
        let components = component_storage::load(&self.directories).await?;
        let dependencies = dependency_storage::load(&self.directories).await?;
        self.publish(
            current.component_catalog.clone(),
            current.dependency_catalog.clone(),
            components,
            dependencies,
        );
        Ok(())
    }

    pub(crate) fn component_steps(&self, component: &Component) -> Vec<InstallStep> {
        self.state().component_steps(component)
    }

    fn publish(
        &self,
        component_catalog: Option<Arc<ComponentCatalog>>,
        dependency_catalog: Option<Arc<DependencyCatalog>>,
        components: Vec<Component>,
        dependencies: Vec<Dependency>,
    ) {
        self.published.send_replace(Arc::new(LibraryState::new(
            component_catalog,
            dependency_catalog,
            components,
            dependencies,
        )));
    }
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

#[cfg(test)]
mod tests {
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
}
