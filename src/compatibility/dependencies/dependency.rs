use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use super::catalog::{CatalogDependencyEntry, DependencyResource};
use crate::{
    compatibility::{
        LibraryError,
        installer::{InstallResource, Installable},
    },
    error::Result,
};

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Dependency {
    pub(super) id: NonNilUuid,
    pub(super) name: String,
    pub(super) version: String,
    #[serde(skip)]
    pub(crate) resources: Vec<DependencyResource>,
}

impl TryFrom<&CatalogDependencyEntry> for Dependency {
    type Error = LibraryError;

    fn try_from(entry: &CatalogDependencyEntry) -> std::result::Result<Self, Self::Error> {
        if entry.resources().is_empty() {
            return Err(LibraryError::UnsupportedDependency(entry.uuid()));
        }
        Ok(Self {
            id: NonNilUuid::new(entry.uuid()).expect("catalog UUID is non-nil"),
            name: entry.name().to_string(),
            version: entry.version().to_string(),
            resources: entry.resources().to_vec(),
        })
    }
}

impl Dependency {
    pub fn id(&self) -> Uuid {
        self.id.get()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }
}

impl Installable for Dependency {
    fn prepare(&self, context: &crate::Context) -> Result<Vec<InstallResource>> {
        let root = context.directories().dependency(self.id());
        self.resources
            .iter()
            .map(|resource| {
                let source = root.join(resource.file_name());
                Ok(InstallResource {
                    source,
                    steps: resource.steps().to_vec(),
                })
            })
            .collect()
    }
}
