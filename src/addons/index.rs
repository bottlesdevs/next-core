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
    catalog::{Catalog, CatalogKind},
};
use crate::{
    Directories,
    error::Result,
    runner::{RunnerKind, detect_runner_kind},
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
    pub(crate) addons: HashMap<Uuid, Arc<Addon<K>>>,
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
        K: CatalogKind,
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
        K: CatalogKind,
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
                    Arc::new(Addon::new_component(
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

fn single_path_component(value: impl AsRef<Path>) -> bool {
    let mut components = value.as_ref().components();
    matches!(components.next(), Some(PathComponent::Normal(_))) && components.next().is_none()
}

async fn regular_file(path: &Path) -> bool {
    async_fs::metadata(path)
        .await
        .is_ok_and(|entry| entry.is_file())
}
