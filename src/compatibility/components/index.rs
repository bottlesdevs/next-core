use std::{collections::HashMap, path::Path};

use futures_lite::StreamExt;
use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use super::{Component, catalog::ComponentKind};
use crate::{Directories, error::Result, runner::detect_runner_kind};

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize, Config)]
#[config(version = 1)]
pub(crate) struct ComponentIndex {
    #[serde(default, rename = "component")]
    components: Vec<Component>,
}

impl ComponentIndex {
    async fn load(directories: &Directories) -> Result<Option<Self>> {
        let path = directories.component_index();
        if async_fs::metadata(&path)
            .await
            .is_ok_and(|entry| entry.is_file())
        {
            Ok(Some(next_config::load(path).await?))
        } else {
            Ok(None)
        }
    }

    async fn save(&self, directories: &Directories) -> Result<()> {
        next_config::save(directories.component_index(), self).await?;
        Ok(())
    }

    pub(crate) async fn record(directories: &Directories, component: Component) -> Result<()> {
        let mut index = Self::load(directories).await?.unwrap_or_default();
        index
            .components
            .retain(|entry| entry.id() != component.id() && entry.path() != component.path());
        index.components.push(component);
        index.save(directories).await
    }
}

pub(crate) async fn scan(directories: &Directories) -> Result<Vec<Component>> {
    let components_path = async_fs::canonicalize(directories.components()).await?;
    let index = ComponentIndex::load(directories).await?;
    let has_index = index.is_some();
    let index = index.unwrap_or_default();
    let components = discover_components(&components_path, &index).await?;
    let component_index = ComponentIndex {
        components: components.clone(),
    };
    if component_index != index || !has_index {
        component_index.save(directories).await?;
    }
    Ok(components)
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
    let mut categories = async_fs::read_dir(components_path).await?;
    while let Some(category_entry) = categories.try_next().await? {
        let file_type = category_entry.file_type().await?;
        if !file_type.is_dir() {
            continue;
        }
        let category_name = category_entry.file_name().to_string_lossy().into_owned();
        let mut versions = async_fs::read_dir(category_entry.path()).await?;
        while let Some(version) = versions.try_next().await? {
            if !version.file_type().await?.is_dir() {
                continue;
            }
            let path = version.path();
            let Some(kind) = detect_kind(&category_name, &path).await? else {
                continue;
            };
            let (id, version) = match indexed.remove(&path) {
                Some(entry) => (entry.id, entry.version.clone()),
                None => (
                    NonNilUuid::new(Uuid::new_v4()).expect("v4 UUID is non-nil"),
                    version.file_name().to_string_lossy().into_owned(),
                ),
            };
            components.push(Component {
                id,
                version,
                path: async_fs::canonicalize(path).await?,
                kind,
            });
        }
    }
    Ok(components)
}

pub(crate) async fn detect_kind(directory: &str, path: &Path) -> Result<Option<ComponentKind>> {
    Ok(Some(match directory {
        "runners" => ComponentKind::Runner {
            kind: detect_runner_kind(path).await?,
        },
        "winebridge"
            if async_fs::metadata(path.join("bottles-winebridge.exe"))
                .await
                .is_ok_and(|entry| entry.is_file()) =>
        {
            ComponentKind::Winebridge
        }
        "umu"
            if async_fs::metadata(path.join("umu-run"))
                .await
                .is_ok_and(|entry| entry.is_file()) =>
        {
            ComponentKind::Umu
        }
        "dxvk" => ComponentKind::Dxvk,
        "vkd3d" => ComponentKind::Vkd3d,
        "nvapi" => ComponentKind::Nvapi,
        "latency-flex" => ComponentKind::LatencyFlex,
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::Directories;

    #[test]
    fn discovers_component_roots() {
        futures_lite::future::block_on(async {
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
                umu
            );
            fs::remove_dir_all(components_path).unwrap();
        });
    }

    #[test]
    fn discovery_is_scoped_to_the_supplied_root_and_preserves_indexed_ids() {
        futures_lite::future::block_on(async {
            let root =
                std::env::temp_dir().join(format!("bottles-next-components-{}", Uuid::new_v4()));
            let left = Directories::from_path(root.join("left")).unwrap();
            let right = Directories::from_path(root.join("right")).unwrap();
            fs::create_dir_all(left.components().join("dxvk/1")).unwrap();
            fs::create_dir_all(right.components().join("dxvk/1")).unwrap();

            let first = scan(&left).await.unwrap();
            let left_id = first[0].id();
            let second = scan(&left).await.unwrap();
            let right = scan(&right).await.unwrap();

            assert_eq!(second[0].id(), left_id);
            assert_ne!(right[0].id(), left_id);
            fs::remove_dir_all(root).unwrap();
        });
    }
}
