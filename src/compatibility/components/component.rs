use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use super::catalog::{CatalogComponentEntry, ComponentKind};
use crate::{
    Directories,
    compatibility::{
        LibraryError, Target,
        installer::{InstallResource, Installable},
    },
    error::Result,
};

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Component {
    pub(super) id: NonNilUuid,
    pub(super) version: String,
    pub(super) path: PathBuf,
    #[serde(flatten)]
    pub(super) kind: ComponentKind,
}

impl Component {
    pub(crate) async fn from_catalog_entry(
        entry: &CatalogComponentEntry,
        directories: &Directories,
    ) -> Result<Self> {
        let target = Target::current().ok_or(LibraryError::UnsupportedComponent(entry.uuid()))?;
        let artifact = entry
            .artifact_for(target)
            .ok_or(LibraryError::UnsupportedComponent(entry.uuid()))?;
        Ok(Self {
            id: NonNilUuid::new(entry.uuid()).expect("catalog UUID is non-nil"),
            version: entry.version().to_string(),
            path: async_fs::canonicalize(
                directories
                    .component_category(entry.kind())
                    .join(artifact.file_name()),
            )
            .await?,
            kind: entry.kind(),
        })
    }

    #[cfg(test)]
    pub(crate) fn new(
        kind: ComponentKind,
        version: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Result<Self> {
        Ok(Self {
            id: NonNilUuid::new(Uuid::new_v4()).expect("v4 UUID is non-nil"),
            version: version.into(),
            path: crate::utils::absolute_path(path.into())?,
            kind,
        })
    }

    pub fn id(&self) -> Uuid {
        self.id.get()
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> ComponentKind {
        self.kind
    }
}

impl Installable for Component {
    fn prepare(&self, context: &crate::Context) -> Result<Vec<InstallResource>> {
        let steps = context.component_steps(self);
        if steps.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![InstallResource {
            source: self.path().to_path_buf(),
            steps,
        }])
    }
}
