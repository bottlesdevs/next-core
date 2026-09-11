//! Immutable local releases, published and removed together with their payloads.

use super::{
    Addon, AddonError, Component, Dependency, Requirement, Slot,
    catalog::AddonFamily,
    installer::{InstallResource, InstallStep},
};
use crate::{Directories, EnvVars, error::Result};
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
    /// Validates the resource structure and component layout at a staged or published payload.
    pub(crate) async fn validate(&self, payload: &Path) -> Result<()> {
        if self.resources.len() != 1 || !self.resources[0].path.as_os_str().is_empty() {
            return Err(AddonError::InvalidRelease(payload.to_path_buf()).into());
        }
        if !async_fs::metadata(payload).await.is_ok_and(|m| m.is_dir()) {
            return Err(AddonError::PayloadMissing(self.id()).into());
        }
        let marker = match self.slot() {
            Slot::Runner => {
                crate::runner::detect_runner_kind(payload).await?;
                None
            }
            Slot::WineBridge => Some("bottles-winebridge.exe"),
            Slot::Umu => Some("umu-run"),
            _ => None,
        };
        let sources = marker
            .map(Path::new)
            .into_iter()
            .chain(self.recipe().filter_map(|step| match step {
                InstallStep::Copy { source, .. } => Some(source.as_path()),
                _ => None,
            }));
        for source in sources {
            if !async_fs::metadata(payload.join(source))
                .await
                .is_ok_and(|entry| entry.is_file())
            {
                return Err(AddonError::InvalidComponent(payload.to_path_buf()).into());
            }
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
        let mut env_vars = EnvVars::default();
        super::installer::replay_env_vars(&mut env_vars, &resource.steps);
        Self {
            addon: Addon::new(
                id,
                name,
                version,
                requirements,
                env_vars,
                Component { slot },
            ),
            resources: vec![resource],
        }
    }

    /// Component role occupied by this release.
    pub fn slot(&self) -> Slot {
        self.addon.slot()
    }
}
impl Release<Dependency> {
    /// Validates unique resource paths and files at a staged or published payload.
    pub(crate) async fn validate(&self, payload: &Path) -> Result<()> {
        let mut names = std::collections::HashSet::new();
        if self.resources.is_empty() || self.resources.iter().any(|r| !names.insert(&r.path)) {
            return Err(AddonError::InvalidRelease(payload.to_path_buf()).into());
        }
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

    pub(crate) fn new_dependency(
        id: NonNilUuid,
        name: String,
        version: String,
        requirements: Vec<Requirement>,
        resources: Vec<InstallResource>,
    ) -> Self {
        let mut env_vars = EnvVars::default();
        super::installer::replay_env_vars(&mut env_vars, resources.iter().flat_map(|r| &r.steps));
        Self {
            addon: Addon::new(
                id,
                name,
                version,
                requirements,
                env_vars,
                Dependency::default(),
            ),
            resources,
        }
    }
}

impl<K: Serialize + serde::de::DeserializeOwned + 'static> next_config::Config for Release<K> {
    const VERSION: u32 = 1;
}
