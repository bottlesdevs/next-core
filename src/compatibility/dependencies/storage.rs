use super::{Dependency, catalog::DependencyCatalog};
use crate::{Directories, error::Result};

pub(crate) async fn scan(
    directories: &Directories,
    catalog: Option<&DependencyCatalog>,
) -> Result<Vec<Dependency>> {
    let Some(catalog) = catalog else {
        return Ok(Vec::new());
    };
    let mut dependencies = Vec::new();
    for entry in catalog {
        let dependency = Dependency::from(entry);
        let root = directories.dependency(dependency.id());
        let mut available = true;
        for resource in &dependency.resources {
            available &= async_fs::metadata(root.join(resource.file_name()))
                .await
                .is_ok_and(|entry| entry.is_file());
        }
        if available {
            dependencies.push(dependency);
        }
    }
    Ok(dependencies)
}
