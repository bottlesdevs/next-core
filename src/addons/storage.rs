//! Local addon manifests and hand-placed component discovery.
//!
//! Components live at `components/<slot>/<version>/`; dependencies live at
//! `dependencies/<uuid>/`. Catalog downloads retain their UUID in `.addon.toml`.

use std::{
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Arc,
};

use futures_lite::StreamExt;
use next_config::Config;
use serde::{Serialize, de::DeserializeOwned};
use strum::IntoEnumIterator;
use uuid::{NonNilUuid, Uuid};

use super::{
    Addon, AddonError, Component, Dependency, Requirement, Slot,
    catalog::{Catalog, CatalogKind},
    installer::recipe_steps,
};
use crate::{
    Directories,
    error::Result,
    runner::{RunnerKind, detect_runner_kind},
};

pub(crate) const MANIFEST: &str = ".addon.toml";

pub(crate) struct AddonStorage<K> {
    dirs: Directories,
    marker: PhantomData<K>,
}

impl<K> AddonStorage<K> {
    pub(crate) fn new(dirs: Directories) -> Self {
        Self {
            dirs,
            marker: PhantomData,
        }
    }

    pub(crate) async fn save(&self, addon: &Addon<K>, staging_path: &Path) -> Result<()>
    where
        Addon<K>: Config,
    {
        next_config::save(staging_path.join(MANIFEST), addon).await?;
        Ok(())
    }

    async fn load(path: &Path) -> Result<Addon<K>>
    where
        Addon<K>: Config,
    {
        Ok(next_config::load(path.join(MANIFEST)).await?)
    }

    pub(crate) async fn remove(&self, addon: &Addon<K>) -> Result<()> {
        async_fs::remove_dir_all(addon.path()).await?;
        Ok(())
    }

    pub(crate) async fn load_catalog(&self) -> Option<Arc<Catalog<K>>>
    where
        K: CatalogKind,
        Catalog<K>: DeserializeOwned,
    {
        let catalog =
            serde_json::from_slice(&async_fs::read(K::path(&self.dirs)).await.ok()?).ok()?;
        Some(Arc::new(catalog))
    }

    pub(crate) async fn save_catalog(&self, catalog: &Catalog<K>) -> Result<()>
    where
        K: CatalogKind,
        Catalog<K>: Serialize,
    {
        async_fs::write(K::path(&self.dirs), serde_json::to_vec(catalog)?).await?;
        Ok(())
    }
}

impl AddonStorage<Component> {
    pub(crate) async fn scan(&self) -> Result<Vec<Addon<Component>>> {
        let mut addons = Vec::new();
        for slot in Slot::iter() {
            let slot_root = self.dirs.components().join(slot.as_str());
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

                let requirements = self.inspect_release(slot, &path).await?;
                let addon = if manifest_exists(&path).await? {
                    let addon = Self::load(&path).await?;
                    if addon.path() != path
                        || addon.slot() != slot
                        || addon.version() != version
                        || addon.requirements() != requirements
                    {
                        return Err(AddonError::InvalidAddonManifest(path).into());
                    }
                    addon
                } else {
                    let id =
                        Uuid::new_v5(&Uuid::NAMESPACE_URL, path.as_os_str().as_encoded_bytes());
                    let addon = Addon::new_component(
                        NonNilUuid::new(id).expect("v5 UUID is non-nil"),
                        version.clone(),
                        version,
                        slot,
                        requirements,
                        path.clone(),
                        recipe_steps(slot).to_vec(),
                    );
                    self.save(&addon, &path).await?;
                    addon
                };
                addons.push(addon);
            }
        }
        Ok(addons)
    }

    pub(crate) async fn target(&self, slot: Slot, version: &str) -> Result<PathBuf> {
        let slot_root = self.dirs.components().join(slot.as_str());
        async_fs::create_dir_all(&slot_root).await?;
        Ok(async_fs::canonicalize(slot_root).await?.join(version))
    }

    pub(crate) async fn inspect_release(
        &self,
        slot: Slot,
        path: &Path,
    ) -> Result<Vec<Requirement>> {
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

impl AddonStorage<Dependency> {
    pub(crate) async fn scan(&self) -> Result<Vec<Addon<Dependency>>> {
        let mut addons = Vec::new();
        let mut entries = async_fs::read_dir(self.dirs.dependencies()).await?;
        while let Some(directory) = entries.try_next().await? {
            if !directory.file_type().await?.is_dir() {
                continue;
            }
            let Ok(id) = directory.file_name().to_string_lossy().parse::<Uuid>() else {
                continue;
            };
            let path = async_fs::canonicalize(directory.path()).await?;
            if !manifest_exists(&path).await? {
                continue;
            }
            let addon = Self::load(&path).await?;
            if addon.path() != path || addon.id() != id || addon.artifacts().is_empty() {
                return Err(AddonError::InvalidAddonManifest(path).into());
            }
            addons.push(addon);
        }
        Ok(addons)
    }

    pub(crate) async fn target(&self, id: Uuid) -> Result<PathBuf> {
        let root = self.dirs.dependencies();
        async_fs::create_dir_all(&root).await?;
        Ok(async_fs::canonicalize(root).await?.join(id.to_string()))
    }
}

async fn manifest_exists(path: &Path) -> Result<bool> {
    match async_fs::metadata(path.join(MANIFEST)).await {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(AddonError::InvalidAddonManifest(path.to_path_buf()).into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn regular_file(path: &Path) -> bool {
    async_fs::metadata(path)
        .await
        .is_ok_and(|entry| entry.is_file())
}
