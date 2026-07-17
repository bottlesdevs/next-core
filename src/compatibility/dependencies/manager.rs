use std::fs;

use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use super::{Dependency, catalog::CatalogDependencyEntry};
use crate::{compatibility::Architecture, error::Result};

#[derive(Debug, Default, Deserialize, Serialize, Config)]
#[config(version = 1)]
struct DependencyIndex {
    #[serde(default)]
    dependencies: Vec<CatalogDependencyEntry>,
}

pub struct DependencyManager {
    dependencies: Vec<Dependency>,
}

impl DependencyManager {
    pub fn new() -> Result<Self> {
        let root = crate::utils::directories::expect().dependencies();
        fs::create_dir_all(&root)?;
        let root = fs::canonicalize(root)?;
        let index_path = root.join("index.toml");
        let index = if index_path.is_file() {
            next_config::load(&index_path)?
        } else {
            let index = DependencyIndex::default();
            next_config::save(&index_path, &index)?;
            index
        };

        let mut dependencies = Vec::with_capacity(index.dependencies.len());
        for entry in index.dependencies {
            let id = entry.uuid();
            let resources = entry
                .resources()
                .iter()
                .filter(|resource| {
                    matches!(
                        resource.target_arch(),
                        Architecture::X86 | Architecture::X86_64
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            if resources.is_empty()
                || resources.iter().any(|resource| {
                    !root
                        .join(id.to_string())
                        .join(resource.file_name())
                        .is_file()
                })
            {
                continue;
            }
            dependencies.push(Dependency {
                id: NonNilUuid::new(id).expect("catalog UUID is non-nil"),
                name: entry.name().to_string(),
                version: entry.version().to_string(),
                resources,
            });
        }
        Ok(Self { dependencies })
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }

    pub fn dependency(&self, id: Uuid) -> Option<&Dependency> {
        self.dependencies
            .iter()
            .find(|dependency| dependency.id() == id)
    }
}
