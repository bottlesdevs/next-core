use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::NonNilUuid;

use super::{Dependency, catalog::CatalogDependencyEntry};
use crate::{Directories, compatibility::Architecture, error::Result};

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize, Config)]
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
    let mut available_entries = Vec::with_capacity(index.dependencies.len());
    for entry in &index.dependencies {
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
        available_entries.push(entry.clone());
    }
    let available_index = DependencyIndex {
        dependencies: available_entries,
    };
    if available_index != index {
        next_config::save(index_path, &available_index).await?;
    }
    Ok(dependencies)
}

pub(crate) async fn record(
    directories: &Directories,
    dependency: CatalogDependencyEntry,
) -> Result<()> {
    let path = directories.dependencies().join("index.toml");
    let mut index: DependencyIndex = next_config::load(&path).await?;
    index
        .dependencies
        .retain(|entry| entry.uuid() != dependency.uuid());
    index.dependencies.push(dependency);
    next_config::save(path, &index).await?;
    Ok(())
}

pub(crate) async fn remove(directories: &Directories, id: uuid::Uuid) -> Result<()> {
    let path = directories.dependencies().join("index.toml");
    let mut index: DependencyIndex = next_config::load(&path).await?;
    index.dependencies.retain(|entry| entry.uuid() != id);
    next_config::save(path, &index).await?;
    Ok(())
}
