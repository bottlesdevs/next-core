use std::{collections::HashMap, path::Path};

use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use super::{Component, catalog::ComponentKind};
use crate::{Context, error::Result, runner::detect_runner_kind};

pub struct ComponentManager {
    components: Vec<Component>,
}

impl ComponentManager {
    pub(crate) async fn load(context: Context) -> Result<Self> {
        let directories = context.directories().clone();
        let component_dir = directories.components();
        let components_path = tokio::fs::canonicalize(component_dir).await?;
        let index_path = components_path.join("index.toml");
        let has_index = tokio::fs::metadata(&index_path)
            .await
            .is_ok_and(|entry| entry.is_file());
        let index = if has_index {
            next_config::load(&index_path).await?
        } else {
            ComponentIndex::default()
        };
        let components = discover_components(&components_path, &index).await?;
        let component_index = ComponentIndex {
            components: components.clone(),
        };
        if component_index != index || !has_index {
            next_config::save(index_path, &component_index).await?;
        }
        Ok(Self { components })
    }

    pub fn components(&self) -> &[Component] {
        &self.components
    }

    pub fn component(&self, id: Uuid) -> Option<&Component> {
        self.components
            .iter()
            .find(|component| component.id() == id)
    }
}

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize, Config)]
#[config(version = 1)]
struct ComponentIndex {
    #[serde(default, rename = "component")]
    components: Vec<Component>,
}

async fn discover_components(
    components_path: &Path,
    index: &ComponentIndex,
) -> Result<Vec<Component>> {
    let mut indexed: HashMap<_, _> = index
        .components
        .iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();

    let mut components = Vec::new();
    let mut entries = tokio::fs::read_dir(components_path).await?;
    let mut categories = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        categories.push(entry);
    }
    categories.sort_by_key(tokio::fs::DirEntry::file_name);
    for category_entry in categories {
        let file_type = category_entry.file_type().await?;
        if !file_type.is_dir() {
            continue;
        }
        let category_name = category_entry.file_name().to_string_lossy().into_owned();
        let mut entries = tokio::fs::read_dir(category_entry.path()).await?;
        let mut versions = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            versions.push(entry);
        }
        versions.sort_by_key(tokio::fs::DirEntry::file_name);
        for version in versions {
            if !version.file_type().await?.is_dir() {
                continue;
            }
            let Some((kind, path)) = component(&category_name, &version.path()).await? else {
                continue;
            };
            let relative = path.to_path_buf();
            let (id, version) = match indexed.remove(&relative) {
                Some(entry) => (entry.id, entry.version.clone()),
                None => (
                    NonNilUuid::new(Uuid::new_v4()).expect("v4 UUID is non-nil"),
                    version.file_name().to_string_lossy().into_owned(),
                ),
            };
            components.push(Component {
                id,
                version,
                path: tokio::fs::canonicalize(path).await?,
                kind,
            });
        }
    }
    components.sort_by(|a, b| a.path().cmp(b.path()));
    Ok(components)
}

async fn component(
    directory: &str,
    path: &Path,
) -> Result<Option<(ComponentKind, std::path::PathBuf)>> {
    Ok(Some(match directory {
        "runners" => (
            ComponentKind::Runner {
                kind: detect_runner_kind(path).await?,
            },
            path.to_path_buf(),
        ),
        "winebridge"
            if tokio::fs::metadata(path.join("bottles-winebridge.exe"))
                .await
                .is_ok_and(|entry| entry.is_file()) =>
        {
            (
                ComponentKind::Winebridge,
                path.join("bottles-winebridge.exe"),
            )
        }
        "umu"
            if tokio::fs::metadata(path.join("umu-run"))
                .await
                .is_ok_and(|entry| entry.is_file()) =>
        {
            (ComponentKind::Umu, path.join("umu-run"))
        }
        "dxvk" => (ComponentKind::Dxvk, path.to_path_buf()),
        "vkd3d" => (ComponentKind::Vkd3d, path.to_path_buf()),
        "nvapi" => (ComponentKind::Nvapi, path.to_path_buf()),
        "latency-flex" => (ComponentKind::LatencyFlex, path.to_path_buf()),
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{Context, Directories};

    #[tokio::test]
    async fn discovers_extracted_components_and_executable_paths() {
        let components_path =
            std::env::temp_dir().join(format!("bottles-next-components-{}", Uuid::new_v4()));
        let winebridge = components_path.join("winebridge/bridge-1");
        let umu = components_path.join("umu/umu-1");
        fs::create_dir_all(&winebridge).unwrap();
        fs::create_dir_all(&umu).unwrap();
        fs::create_dir_all(components_path.join("dxvk/dxvk-1")).unwrap();
        fs::write(winebridge.join("bottles-winebridge.exe"), []).unwrap();
        fs::write(umu.join("umu-run"), []).unwrap();

        let components = discover_components(&components_path, &ComponentIndex::default())
            .await
            .unwrap();

        assert_eq!(components.len(), 3);
        assert!(
            components
                .iter()
                .any(|component| component.kind() == ComponentKind::Winebridge)
        );
        assert!(
            components
                .iter()
                .any(|component| component.kind() == ComponentKind::Umu)
        );
        assert!(
            components
                .iter()
                .any(|component| component.kind() == ComponentKind::Dxvk)
        );
        assert_eq!(
            components
                .iter()
                .find(|component| component.kind() == ComponentKind::Umu)
                .unwrap()
                .path(),
            umu.join("umu-run")
        );

        fs::remove_dir_all(components_path).unwrap();
    }

    #[tokio::test]
    async fn discovery_is_scoped_to_the_supplied_root_and_preserves_indexed_ids() {
        let root = std::env::temp_dir().join(format!("bottles-next-components-{}", Uuid::new_v4()));
        let left = Directories::from_path(root.join("left")).unwrap();
        let right = Directories::from_path(root.join("right")).unwrap();
        fs::create_dir_all(left.components().join("dxvk/1")).unwrap();
        fs::create_dir_all(right.components().join("dxvk/1")).unwrap();

        let left_context = Context::new(left.clone(), left.data_dir().join("fvs2d")).unwrap();
        let right_context = Context::new(right.clone(), right.data_dir().join("fvs2d")).unwrap();
        let first = ComponentManager::load(left_context.clone()).await.unwrap();
        let left_id = first.components()[0].id();
        let second = ComponentManager::load(left_context).await.unwrap();
        let right = ComponentManager::load(right_context).await.unwrap();

        assert_eq!(second.components()[0].id(), left_id);
        assert_ne!(right.components()[0].id(), left_id);
        fs::remove_dir_all(root).unwrap();
    }
}
