//! Reconciles persisted addon indexes with managed storage.
//!
//! Component releases can be identified from their slot directory and contents,
//! so their index is rebuilt from disk. Dependency identities and recipes cannot
//! be reconstructed; rebuilding that family only validates persisted metadata.

use std::{
    collections::HashMap,
    path::{Component as PathComponent, Path, PathBuf},
    sync::Arc,
};

use futures_lite::StreamExt;
use strum::IntoEnumIterator;
use uuid::{NonNilUuid, Uuid};

use crate::{
    Directories,
    error::Result,
    runner::{RunnerKind, detect_runner_kind},
};

use super::super::{AddonError, Component, Dependency, Requirement, Slot, catalog::AddonFamily};
use super::{AddonIndex, IndexEntry};

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

    /// Reconciles indexed records with component directories.
    ///
    /// Matching slot/version records keep their catalog UUID. New directories
    /// receive a deterministic path-derived UUID, and records missing from disk
    /// are dropped. A retained record is rejected when its derived requirements
    /// have changed, because silently keeping its catalog identity would attach
    /// that identity to different contents.
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

    /// Validates the files that identify `slot` and derives requirements from them.
    pub(crate) async fn inspect_release(slot: Slot, path: &Path) -> Result<Vec<Requirement>> {
        let invalid = || AddonError::InvalidComponent(path.to_path_buf());
        match slot {
            Slot::Runner => Ok(match detect_runner_kind(path).await? {
                RunnerKind::Wine | RunnerKind::Gptk => Vec::new(),
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
    /// Loads the dependency index and validates its persisted record structure.
    ///
    /// This does not discover dependencies or verify artifact files on disk;
    /// neither dependency identity nor installation recipes can be reconstructed
    /// from storage alone.
    pub(crate) async fn load(directories: &Directories) -> Result<Self> {
        let mut index = Self::open(directories).await?;
        let previous = index.addons.clone();
        index.rebuild(directories).await?;
        if index.addons != previous {
            index.save(directories).await?;
        }
        Ok(index)
    }

    /// Rejects records with mismatched UUIDs, no artifacts, or unsafe artifact paths.
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
