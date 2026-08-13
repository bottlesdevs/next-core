//! Central addon indexes and hand-placed component discovery.
//!
//! Components live at `components/<slot>/<version>/`; dependencies live at
//! `dependencies/<uuid>/`.
//!
//! Component directories are discoverable, so rebuilding adds hand-placed
//! components and removes entries whose directories disappeared. Dependency
//! recipes cannot be reconstructed from their files; only indexed dependencies
//! are recognized. Catalogs remain separate JSON documents and are never
//! serialized into an index.

use std::{
    collections::HashMap,
    path::{Component as PathComponent, Path, PathBuf},
    sync::Arc,
};

use futures_lite::StreamExt;
use next_config::Config;
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use uuid::{NonNilUuid, Uuid};

use super::{
    Addon, AddonError, Component, Dependency, Requirement, Slot,
    catalog::{AddonFamily, Catalog},
    item::Artifact,
};
use crate::{
    Directories,
    error::Result,
    runner::{Runner, RunnerKind, detect_runner_kind},
};

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

impl AddonIndex<Component> {
    /// Loads and rebuilds the component index, persisting it only when changed.
    pub(crate) async fn load(directories: &Directories) -> Result<Self> {
        let mut index = Self::open(directories).await?;
        let previous = index.addons.clone();
        index.rebuild(directories).await?;
        if index.addons != previous {
            index.save(directories).await?;
        }
        Ok(index)
    }

    /// Reconstructs the component set from managed slot directories.
    ///
    /// Existing records retain their catalog UUID when their slot, version, and
    /// derived requirements agree with disk. Unindexed directories become
    /// hand-placed components with path-derived UUIDs, while records without a
    /// directory are dropped.
    async fn rebuild(&mut self, directories: &Directories) -> Result<()> {
        let index_path = Component::index(directories);
        let root = async_fs::canonicalize(directories.components()).await?;
        let mut indexed = HashMap::new();
        for (id, addon) in &self.addons {
            if *id != addon.id() {
                return Err(AddonError::InvalidAddonIndex(index_path).into());
            }
            if indexed
                .insert((addon.slot(), addon.version().to_owned()), addon.clone())
                .is_some()
            {
                return Err(AddonError::InvalidAddonIndex(index_path).into());
            }
        }

        let mut addons = HashMap::new();
        for slot in Slot::iter() {
            let slot_root = root.join(slot.as_str());
            let mut versions = match async_fs::read_dir(slot_root).await {
                Ok(versions) => versions,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            while let Some(version) = versions.try_next().await? {
                if !version.file_type().await?.is_dir() {
                    continue;
                }
                let path = async_fs::canonicalize(version.path()).await?;
                let version = version.file_name().to_string_lossy().into_owned();
                if slot != Slot::Runner && semver::Version::parse(&version).is_err() {
                    return Err(AddonError::InvalidComponent(path).into());
                }

                let requirements = Self::inspect_release(slot, &path).await?;
                let addon = if let Some(addon) = indexed.remove(&(slot, version.clone())) {
                    if addon.requirements() != requirements {
                        return Err(AddonError::InvalidAddonIndex(index_path).into());
                    }
                    addon
                } else {
                    let id =
                        Uuid::new_v5(&Uuid::NAMESPACE_URL, path.as_os_str().as_encoded_bytes());
                    Arc::new(IndexEntry::new_component(
                        NonNilUuid::new(id).expect("v5 UUID is non-nil"),
                        version.clone(),
                        version,
                        slot,
                        requirements,
                    ))
                };
                if addons.insert(addon.id(), addon.clone()).is_some() {
                    return Err(AddonError::Duplicate(addon.id()).into());
                }
            }
        }
        self.addons = addons;
        Ok(())
    }

    pub(crate) async fn target(
        directories: &Directories,
        slot: Slot,
        version: &str,
    ) -> Result<PathBuf> {
        let slot_root = directories.components().join(slot.as_str());
        async_fs::create_dir_all(&slot_root).await?;
        Ok(async_fs::canonicalize(slot_root).await?.join(version))
    }

    /// Validates slot-specific files and derives requirements available without a catalog.
    pub(crate) async fn inspect_release(slot: Slot, path: &Path) -> Result<Vec<Requirement>> {
        let invalid = || AddonError::InvalidComponent(path.to_path_buf());
        match slot {
            Slot::Runner => Ok(match detect_runner_kind(path).await? {
                RunnerKind::Wine => Vec::new(),
                RunnerKind::Proton => vec![Requirement::Slot(Slot::Umu)],
            }),
            Slot::WineBridge => {
                if !regular_file(&path.join("bottles-winebridge.exe")).await {
                    return Err(invalid().into());
                }
                Ok(Vec::new())
            }
            Slot::Umu => {
                if !regular_file(&path.join("umu-run")).await {
                    return Err(invalid().into());
                }
                Ok(Vec::new())
            }
            Slot::Nvapi => Ok(vec![Requirement::Slot(Slot::Dxvk)]),
            Slot::Dxvk | Slot::Vkd3d | Slot::LatencyFlex => Ok(Vec::new()),
        }
    }
}

impl AddonIndex<Dependency> {
    /// Loads the dependency index and removes records that are no longer complete.
    pub(crate) async fn load(directories: &Directories) -> Result<Self> {
        let mut index = Self::open(directories).await?;
        let previous = index.addons.clone();
        index.rebuild(directories).await?;
        if index.addons != previous {
            index.save(directories).await?;
        }
        Ok(index)
    }

    /// Retains valid indexed dependencies whose directory and every artifact exist.
    ///
    /// Unindexed directories are intentionally ignored because their requirements
    /// and installation recipes cannot be derived from downloaded files.
    async fn rebuild(&mut self, directories: &Directories) -> Result<()> {
        let index_path = Dependency::index(directories);
        let mut addons = HashMap::new();
        for (id, addon) in &self.addons {
            if *id != addon.id()
                || addon.artifacts().is_empty()
                || addon
                    .artifacts()
                    .iter()
                    .any(|artifact| !single_path_component(&artifact.path))
            {
                return Err(AddonError::InvalidAddonIndex(index_path).into());
            }
            addons.insert(*id, addon.clone());
        }
        self.addons = addons;
        Ok(())
    }

    pub(crate) async fn target(directories: &Directories, id: Uuid) -> Result<PathBuf> {
        let path = directories.dependencies();
        async_fs::create_dir_all(&path).await?;
        Ok(async_fs::canonicalize(path).await?.join(id.to_string()))
    }
}

/// A complete downloaded or hand-placed addon snapshot.
///
/// `K` is either [`Component`] or [`Dependency`]. Values do not update after
/// downloads, removals, or catalog refreshes; query [`crate::Addons`] again to
/// observe later state. Local paths are derived from the active Bottles data
/// directory.
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

    /// Returns the artifact-free addon metadata stored in bottle state.
    pub fn addon(&self) -> &Addon<K> {
        &self.addon
    }

    /// Returns the immutable release identifier.
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

    /// Returns the requirements checked before a bottle mutation.
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

fn single_path_component(value: impl AsRef<Path>) -> bool {
    let mut components = value.as_ref().components();
    matches!(components.next(), Some(PathComponent::Normal(_))) && components.next().is_none()
}

async fn regular_file(path: &Path) -> bool {
    async_fs::metadata(path)
        .await
        .is_ok_and(|entry| entry.is_file())
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
