//! Immutable local releases, published and removed together with their payloads.

use super::{
    Addon, AddonError, Component, Dependency, Requirement, Slot,
    catalog::AddonFamily,
    installer::{InstallResource, InstallStep},
};
use crate::{Directories, error::Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::{NonNilUuid, Uuid};

/// An immutable local release, stored with its source payload.
/// Returned handles are metadata snapshots; paths use the active data directory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound(serialize = "K: Serialize", deserialize = "K: Deserialize<'de>"))]
pub struct Release<K> {
    #[serde(flatten)]
    addon: Addon<K>,
    resources: Vec<InstallResource>,
}

impl<K> Release<K> {
    /// Selection metadata suitable for owner state.
    pub fn addon(&self) -> &Addon<K> {
        &self.addon
    }
    /// Immutable release identity.
    pub fn id(&self) -> Uuid {
        self.addon.id()
    }
    /// Release display name.
    pub fn name(&self) -> &str {
        self.addon.name()
    }
    /// Release version attribute.
    pub fn version(&self) -> &str {
        self.addon.version()
    }
    /// Required coexisting addons.
    pub fn requirements(&self) -> &[Requirement] {
        self.addon.requirements()
    }
    pub(crate) fn path(&self, directories: &Directories) -> PathBuf
    where
        K: AddonFamily,
    {
        self.directory(directories).join("payload")
    }
    pub(super) fn directory(&self, directories: &Directories) -> PathBuf
    where
        K: AddonFamily,
    {
        K::releases(directories).join(self.id().to_string())
    }
    pub(crate) fn resources(&self) -> &[InstallResource] {
        &self.resources
    }
    pub(crate) fn recipe(&self) -> impl DoubleEndedIterator<Item = &InstallStep> {
        self.resources.iter().flat_map(|r| &r.steps)
    }
}
impl<K: Clone> From<&Release<K>> for Addon<K> {
    fn from(entry: &Release<K>) -> Self {
        entry.addon.clone()
    }
}
impl Release<Component> {
    pub(crate) async fn require_payload(&self, directories: &Directories) -> Result<()> {
        if !async_fs::metadata(self.path(directories))
            .await
            .is_ok_and(|m| m.is_dir())
        {
            return Err(AddonError::PayloadMissing(self.id()).into());
        }
        Ok(())
    }

    pub(super) fn validate(&self, path: &Path) -> Result<()> {
        if self.resources.len() != 1 || !self.resources[0].path.as_os_str().is_empty() {
            return Err(AddonError::InvalidRelease(path.to_path_buf()).into());
        }
        Ok(())
    }

    pub(crate) fn new_component(
        id: NonNilUuid,
        name: String,
        version: String,
        slot: Slot,
        requirements: Vec<Requirement>,
        resource: InstallResource,
    ) -> Self {
        Self {
            addon: Addon::new(id, name, version, requirements, Component { slot }),
            resources: vec![resource],
        }
    }

    /// Component role occupied by this release.
    pub fn slot(&self) -> Slot {
        self.addon.slot()
    }
}
impl Release<Dependency> {
    pub(crate) async fn require_payload(&self, directories: &Directories) -> Result<()> {
        let payload = self.path(directories);
        for resource in &self.resources {
            if !async_fs::metadata(payload.join(&resource.path))
                .await
                .is_ok_and(|m| m.is_file())
            {
                return Err(AddonError::PayloadMissing(self.id()).into());
            }
        }
        Ok(())
    }

    pub(super) fn validate(&self, path: &Path) -> Result<()> {
        let mut names = std::collections::HashSet::new();
        if self.resources.is_empty()
            || self
                .resources
                .iter()
                .any(|r| !single_name(&r.path) || !names.insert(&r.path))
        {
            return Err(AddonError::InvalidRelease(path.to_path_buf()).into());
        }
        Ok(())
    }

    pub(crate) fn new_dependency(
        id: NonNilUuid,
        name: String,
        version: String,
        requirements: Vec<Requirement>,
        resources: Vec<InstallResource>,
    ) -> Self {
        Self {
            addon: Addon::new(id, name, version, requirements, Dependency::default()),
            resources,
        }
    }
}

impl<K: Serialize + serde::de::DeserializeOwned + 'static> next_config::Config for Release<K> {
    const VERSION: u32 = 1;
}
fn single_name(path: &Path) -> bool {
    let mut parts = path.components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn id() -> NonNilUuid {
        NonNilUuid::new(Uuid::new_v4()).unwrap()
    }

    #[test]
    fn release_flattens_addon_and_rejects_unknown_fields() {
        let entry = Release::new_dependency(
            id(),
            "dependency".into(),
            "1.0.0".into(),
            vec![Requirement::Slot(Slot::Runner)],
            vec![InstallResource::new("setup.exe", Vec::new())],
        );
        let value = serde_json::to_value(&entry).unwrap();

        assert!(value.get("addon").is_none());
        assert_eq!(value["name"], "dependency");
        assert_eq!(value["resources"][0]["path"], "setup.exe");
        assert_eq!(
            serde_json::from_value::<Release<Dependency>>(value.clone()).unwrap(),
            entry
        );

        let addon = Addon::from(&entry);
        let addon_value = serde_json::to_value(&addon).unwrap();
        assert!(addon_value.get("resources").is_none());
        assert_eq!(addon.id(), entry.id());
        assert_eq!(addon.requirements(), entry.requirements());

        let mut unknown_entry = value;
        unknown_entry
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), Value::Bool(true));
        assert!(serde_json::from_value::<Release<Dependency>>(unknown_entry).is_err());

        let mut unknown_addon = addon_value;
        unknown_addon
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), Value::Bool(true));
        assert!(serde_json::from_value::<Addon<Dependency>>(unknown_addon).is_err());
    }

    #[test]
    fn release_paths_use_active_directories() {
        let root = std::env::temp_dir().join(format!("bottles-next-{}", Uuid::new_v4()));
        let directories = Directories::from_path(&root).unwrap();
        let component = Release::new_component(
            id(),
            "runner".into(),
            "1.0.0".into(),
            Slot::Runner,
            Vec::new(),
            InstallResource::new("", Vec::new()),
        );
        let dependency = Release::new_dependency(
            id(),
            "dependency".into(),
            "1.0.0".into(),
            Vec::new(),
            vec![InstallResource::new("setup.exe", Vec::new())],
        );

        assert_eq!(
            component
                .path(&directories)
                .join(&component.resources()[0].path),
            directories
                .components()
                .join("releases")
                .join(component.id().to_string())
                .join("payload")
        );
        assert_eq!(
            dependency
                .path(&directories)
                .join(&dependency.resources()[0].path),
            directories
                .dependencies()
                .join("releases")
                .join(dependency.id().to_string())
                .join("payload/setup.exe")
        );
        let serialized = serde_json::to_string(&(component, dependency)).unwrap();
        assert!(!serialized.contains(root.to_string_lossy().as_ref()));
    }
}
