use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use super::catalog::DependencyResource;
use crate::{
    compatibility::{
        Architecture,
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
    fn prepare(&self, directories: &crate::Directories) -> Result<Vec<InstallResource>> {
        let root = directories.dependencies().join(self.id().to_string());
        self.resources
            .iter()
            .filter(|resource| {
                matches!(
                    resource.target_arch(),
                    Architecture::X86 | Architecture::X86_64
                )
            })
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
