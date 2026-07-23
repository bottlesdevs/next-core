use std::{collections::HashMap, fs, io, path::Path};

use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use super::{Component, catalog::ComponentKind};
use crate::{Directories, error::Result, runner::detect_runner_kind};

pub struct ComponentManager {
    components: Vec<Component>,
}

impl ComponentManager {
    pub fn load(directories: &Directories) -> Result<Self> {
        let component_dir = directories.components();
        fs::create_dir_all(&component_dir)?;
        let components_path = fs::canonicalize(component_dir)?;
        let index_path = components_path.join("index.toml");
        let index = if index_path.is_file() {
            next_config::load(&index_path)?
        } else {
            ComponentIndex::default()
        };

        let components = discover_components(&components_path, &index)?;
        let component_index = ComponentIndex {
            components: components.clone(),
        };
        if component_index != index || !index_path.is_file() {
            next_config::save(index_path, &component_index)?;
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

fn discover_components(components_path: &Path, index: &ComponentIndex) -> Result<Vec<Component>> {
    let mut indexed: HashMap<_, _> = index
        .components
        .iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();

    let mut components = Vec::new();
    let mut categories = fs::read_dir(components_path)?.collect::<io::Result<Vec<_>>>()?;
    categories.sort_by_key(fs::DirEntry::file_name);
    for category_entry in categories {
        let file_type = category_entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }
        let category_name = category_entry.file_name().to_string_lossy().into_owned();
        let mut versions = fs::read_dir(category_entry.path())?.collect::<io::Result<Vec<_>>>()?;
        versions.sort_by_key(fs::DirEntry::file_name);
        for version in versions {
            if !version.file_type()?.is_dir() {
                continue;
            }
            let Some((kind, path)) = component(&category_name, &version.path())? else {
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
                path: fs::canonicalize(path)?,
                kind,
            });
        }
    }
    components.sort_by(|a, b| a.path().cmp(b.path()));
    Ok(components)
}

fn component(directory: &str, path: &Path) -> Result<Option<(ComponentKind, std::path::PathBuf)>> {
    Ok(Some(match directory {
        "runners" => (
            ComponentKind::Runner {
                kind: detect_runner_kind(path)?,
            },
            path.to_path_buf(),
        ),
        "winebridge" if path.join("bottles-winebridge.exe").is_file() => (
            ComponentKind::Winebridge,
            path.join("bottles-winebridge.exe"),
        ),
        "umu" if path.join("umu-run").is_file() => (ComponentKind::Umu, path.join("umu-run")),
        "dxvk" => (ComponentKind::Dxvk, path.to_path_buf()),
        "vkd3d" => (ComponentKind::Vkd3d, path.to_path_buf()),
        "nvapi" => (ComponentKind::Nvapi, path.to_path_buf()),
        "latency-flex" => (ComponentKind::LatencyFlex, path.to_path_buf()),
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Directories;

    #[test]
    fn discovers_extracted_components_and_executable_paths() {
        let components_path =
            std::env::temp_dir().join(format!("bottles-next-components-{}", Uuid::new_v4()));
        let winebridge = components_path.join("winebridge/bridge-1");
        let umu = components_path.join("umu/umu-1");
        fs::create_dir_all(&winebridge).unwrap();
        fs::create_dir_all(&umu).unwrap();
        fs::create_dir_all(components_path.join("dxvk/dxvk-1")).unwrap();
        fs::write(winebridge.join("bottles-winebridge.exe"), []).unwrap();
        fs::write(umu.join("umu-run"), []).unwrap();

        let components = discover_components(&components_path, &ComponentIndex::default()).unwrap();

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

    #[test]
    fn discovery_is_scoped_to_the_supplied_root_and_preserves_indexed_ids() {
        let root = std::env::temp_dir().join(format!("bottles-next-components-{}", Uuid::new_v4()));
        let left = Directories {
            data_dir: root.join("left"),
            runtime_dir: root.join("left-run"),
        };
        let right = Directories {
            data_dir: root.join("right"),
            runtime_dir: root.join("right-run"),
        };
        fs::create_dir_all(left.components().join("dxvk/1")).unwrap();
        fs::create_dir_all(right.components().join("dxvk/1")).unwrap();

        let first = ComponentManager::load(&left).unwrap();
        let left_id = first.components()[0].id();
        let second = ComponentManager::load(&left).unwrap();
        let right = ComponentManager::load(&right).unwrap();

        assert_eq!(second.components()[0].id(), left_id);
        assert_ne!(right.components()[0].id(), left_id);
        fs::remove_dir_all(root).unwrap();
    }
}
