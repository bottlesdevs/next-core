use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::NonNilUuid;

use super::{Dependency, catalog::CatalogDependencyEntry};
use crate::{Directories, compatibility::Architecture, error::Result};

#[derive(Debug, Default, Deserialize, Serialize, Config)]
#[config(version = 1)]
struct DependencyIndex {
    #[serde(default)]
    dependencies: Vec<CatalogDependencyEntry>,
}

pub(crate) async fn load(directories: &Directories) -> Result<Vec<Dependency>> {
    let root = async_fs::canonicalize(directories.dependencies()).await?;
    let index_path = root.join("index.toml");
    let index = if async_fs::metadata(&index_path)
        .await
        .is_ok_and(|entry| entry.is_file())
    {
        next_config::load(&index_path).await?
    } else {
        let index = DependencyIndex::default();
        next_config::save(&index_path, &index).await?;
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
        let mut available = !resources.is_empty();
        for resource in &resources {
            available &= async_fs::metadata(root.join(id.to_string()).join(resource.file_name()))
                .await
                .is_ok_and(|entry| entry.is_file());
        }
        if !available {
            continue;
        }
        dependencies.push(Dependency {
            id: NonNilUuid::new(id).expect("catalog UUID is non-nil"),
            name: entry.name().to_string(),
            version: entry.version().to_string(),
            resources,
        });
    }
    Ok(dependencies)
}
