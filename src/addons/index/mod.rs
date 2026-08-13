//! Local addon indexes and hand-placed component discovery.
//!
//! Components live at `components/<slot>/<version>/`; dependencies live at
//! `dependencies/<uuid>/`.
//!
//! Component directories are discoverable, so rebuilding adds hand-placed
//! components and removes entries whose directories disappeared. Dependency
//! identity and recipes cannot be reconstructed from downloaded files; their
//! index is therefore authoritative. Catalogs remain separate JSON documents
//! and are never serialized into an index.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use super::{
    Addon, AddonError, Component, Dependency, Requirement, Slot,
    catalog::{AddonFamily, Catalog},
    installer::Artifact,
};
use crate::{Directories, error::Result, runner::Runner};

mod rebuild;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    deny_unknown_fields,
    bound(serialize = "K: Serialize", deserialize = "K: Deserialize<'de>")
)]
/// A snapshot of the catalog and indexed local addons for one category.
///
/// Only `addons` is persisted. UUID keys must match their addon records;
/// release locations are derived from their typed identities.
pub(crate) struct AddonIndex<K> {
    /// The last usable catalog, retained only for the current process snapshot.
    #[serde(skip)]
    pub(crate) catalog: Option<Arc<Catalog<K>>>,
    /// Complete local releases keyed by immutable release UUID.
    #[serde(rename = "addon")]
    pub(crate) addons: HashMap<Uuid, Arc<IndexEntry<K>>>,
}

impl Config for AddonIndex<Component> {
    const VERSION: u32 = 1;
}

impl Config for AddonIndex<Dependency> {
    const VERSION: u32 = 1;
}

impl<K> AddonIndex<K> {
    /// Loads the persisted index without examining addon directories.
    ///
    /// A missing file produces an empty index. An existing non-file or malformed
    /// index is an error because silently rebuilding it could discard catalog
    /// UUIDs and installation recipes.
    async fn open(directories: &Directories) -> Result<Self>
    where
        K: AddonFamily,
        Self: Config,
    {
        let path = K::index(directories);
        match async_fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => Ok(next_config::load(path).await?),
            Ok(_) => Err(AddonError::InvalidAddonIndex(path).into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                catalog: None,
                addons: HashMap::new(),
            }),
            Err(error) => Err(error.into()),
        }
    }

    /// Attaches the current catalog without including it in persisted index data.
    pub(crate) fn with_catalog(mut self, catalog: Arc<Catalog<K>>) -> Self {
        self.catalog = Some(catalog);
        self
    }

    /// Atomically replaces the category index while omitting the runtime catalog.
    pub(crate) async fn save(&self, directories: &Directories) -> Result<()>
    where
        K: AddonFamily,
        Self: Config,
    {
        next_config::save(K::index(directories), self).await?;
        Ok(())
    }
}

/// A downloaded or hand-placed addon recorded in shared storage.
///
/// `K` is either [`Component`] or [`Dependency`]. Values do not update after
/// downloads, removals, or catalog refreshes; query [`crate::Addons`] again to
/// observe later state. Dependency entries retain installation artifacts;
/// converting an entry to [`Addon`] produces the artifact-free value suitable
/// for bottle state. Local paths are derived from the active Bottles data directory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound(serialize = "K: Serialize", deserialize = "K: Deserialize<'de>"))]
pub struct IndexEntry<K> {
    #[serde(flatten)]
    addon: Addon<K>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<Artifact>,
}

impl<K> IndexEntry<K> {
    fn new(addon: Addon<K>, artifacts: Vec<Artifact>) -> Self {
        Self { addon, artifacts }
    }

    /// Returns the artifact-free selection metadata suitable for bottle state.
    pub fn addon(&self) -> &Addon<K> {
        &self.addon
    }

    /// Returns the release identifier shared by its catalog and bottle records.
    pub fn id(&self) -> Uuid {
        self.addon.id()
    }

    /// Returns the catalog label, or version directory name for hand-placed components.
    pub fn name(&self) -> &str {
        self.addon.name()
    }

    /// Returns the downloaded catalog or hand-placed version string.
    pub fn version(&self) -> &str {
        self.addon.version()
    }

    /// Returns the addons that must coexist with this release.
    pub fn requirements(&self) -> &[Requirement] {
        self.addon.requirements()
    }
}

impl<K: Clone> From<&IndexEntry<K>> for Addon<K> {
    fn from(entry: &IndexEntry<K>) -> Self {
        entry.addon.clone()
    }
}

impl IndexEntry<Component> {
    pub(crate) fn new_component(
        id: NonNilUuid,
        name: String,
        version: String,
        slot: Slot,
        requirements: Vec<Requirement>,
    ) -> Self {
        Self::new(
            Addon::new(id, name, version, requirements, Component { slot }),
            Vec::new(),
        )
    }

    /// Returns the mutually exclusive role occupied by this component.
    pub fn slot(&self) -> Slot {
        self.addon.slot()
    }

    pub(crate) fn path(&self, directories: &Directories) -> PathBuf {
        self.addon.path(directories)
    }

    pub(crate) fn artifact(&self, directories: &Directories) -> Artifact {
        self.addon.artifact(directories)
    }

    pub(crate) async fn load_runner(
        &self,
        directories: &Directories,
        umu: Option<&Self>,
    ) -> Result<Box<dyn Runner>> {
        self.addon
            .load_runner(directories, umu.map(|entry| &entry.addon))
            .await
    }
}

impl IndexEntry<Dependency> {
    pub(crate) fn new_dependency(
        id: NonNilUuid,
        name: String,
        version: String,
        requirements: Vec<Requirement>,
        artifacts: Vec<Artifact>,
    ) -> Self {
        Self::new(
            Addon::new(id, name, version, requirements, Dependency::default()),
            artifacts,
        )
    }

    pub(crate) fn artifacts(&self) -> &[Artifact] {
        &self.artifacts
    }

    pub(crate) fn path(&self, directories: &Directories) -> PathBuf {
        directories.dependencies().join(self.id().to_string())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn id() -> NonNilUuid {
        NonNilUuid::new(Uuid::new_v4()).unwrap()
    }

    #[test]
    fn index_entry_flattens_addon_and_rejects_unknown_fields() {
        let entry = IndexEntry::new_dependency(
            id(),
            "dependency".into(),
            "1.0.0".into(),
            vec![Requirement::Slot(Slot::Runner)],
            vec![Artifact::new(PathBuf::from("setup.exe"), Vec::new())],
        );
        let value = serde_json::to_value(&entry).unwrap();

        assert!(value.get("addon").is_none());
        assert_eq!(value["name"], "dependency");
        assert_eq!(value["artifacts"][0]["path"], "setup.exe");
        assert_eq!(
            serde_json::from_value::<IndexEntry<Dependency>>(value.clone()).unwrap(),
            entry
        );

        let addon = Addon::from(&entry);
        let addon_value = serde_json::to_value(&addon).unwrap();
        assert!(addon_value.get("artifacts").is_none());
        assert_eq!(addon.id(), entry.id());
        assert_eq!(addon.requirements(), entry.requirements());

        let mut unknown_entry = value;
        unknown_entry
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), Value::Bool(true));
        assert!(serde_json::from_value::<IndexEntry<Dependency>>(unknown_entry).is_err());

        let mut unknown_addon = addon_value;
        unknown_addon
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), Value::Bool(true));
        assert!(serde_json::from_value::<Addon<Dependency>>(unknown_addon).is_err());
    }

    #[test]
    fn index_paths_use_active_directories() {
        let root = std::env::temp_dir().join(format!("bottles-next-{}", Uuid::new_v4()));
        let directories = Directories::from_path(&root).unwrap();
        let component = IndexEntry::new_component(
            id(),
            "runner".into(),
            "1.0.0".into(),
            Slot::Runner,
            Vec::new(),
        );
        let dependency = IndexEntry::new_dependency(
            id(),
            "dependency".into(),
            "1.0.0".into(),
            Vec::new(),
            vec![Artifact::new(PathBuf::from("setup.exe"), Vec::new())],
        );

        assert_eq!(
            component.path(&directories),
            directories.components().join("runner/1.0.0")
        );
        assert_eq!(
            dependency.path(&directories),
            directories.dependencies().join(dependency.id().to_string())
        );
        let serialized = serde_json::to_string(&(component, dependency)).unwrap();
        assert!(!serialized.contains(root.to_string_lossy().as_ref()));
    }
}
