//! Discovery, atomic commit, and removal of acquired releases.

use super::{Addons, AddonsState};
use crate::{
    Addon, AddonError, Component, Dependency, Directories, Runner, Umu, WineBridge,
    addons::catalog::Catalog,
    error::{Error, Result},
    utils::fs,
};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Projects a typed release from shared storage and selects its directory.
pub(crate) trait StoredAddon: Sized + PartialEq + Send + Sync + 'static {
    fn releases(directories: &Directories) -> PathBuf {
        directories.component_releases()
    }

    fn get(release: &StoredRelease) -> Option<&Arc<Addon<Self>>>;
    fn manifest(record: Arc<Addon<Self>>) -> StoredRelease;
}

/// The discriminator belongs to shared storage, where several kinds share a root.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum StoredRelease {
    Runner(Arc<Addon<Runner>>),
    #[serde(rename = "winebridge")]
    WineBridge(Arc<Addon<WineBridge>>),
    Umu(Arc<Addon<Umu>>),
    Component(Arc<Addon<Component>>),
    Dependency(Arc<Addon<Dependency>>),
}

impl StoredRelease {
    fn id(&self) -> Uuid {
        match self {
            Self::Runner(record) => record.id(),
            Self::WineBridge(record) => record.id(),
            Self::Umu(record) => record.id(),
            Self::Component(record) => record.id(),
            Self::Dependency(record) => record.id(),
        }
    }
}

impl next_config::Config for StoredRelease {
    const VERSION: u32 = 1;
}

impl StoredAddon for Runner {
    fn get(release: &StoredRelease) -> Option<&Arc<Addon<Self>>> {
        match release {
            StoredRelease::Runner(record) => Some(record),
            _ => None,
        }
    }

    fn manifest(record: Arc<Addon<Self>>) -> StoredRelease {
        StoredRelease::Runner(record)
    }
}

impl StoredAddon for WineBridge {
    fn get(release: &StoredRelease) -> Option<&Arc<Addon<Self>>> {
        match release {
            StoredRelease::WineBridge(record) => Some(record),
            _ => None,
        }
    }

    fn manifest(record: Arc<Addon<Self>>) -> StoredRelease {
        StoredRelease::WineBridge(record)
    }
}

impl StoredAddon for Umu {
    fn get(release: &StoredRelease) -> Option<&Arc<Addon<Self>>> {
        match release {
            StoredRelease::Umu(record) => Some(record),
            _ => None,
        }
    }

    fn manifest(record: Arc<Addon<Self>>) -> StoredRelease {
        StoredRelease::Umu(record)
    }
}

impl StoredAddon for Component {
    fn get(release: &StoredRelease) -> Option<&Arc<Addon<Self>>> {
        match release {
            StoredRelease::Component(record) => Some(record),
            _ => None,
        }
    }

    fn manifest(record: Arc<Addon<Self>>) -> StoredRelease {
        StoredRelease::Component(record)
    }
}

impl StoredAddon for Dependency {
    fn releases(directories: &Directories) -> PathBuf {
        directories.dependency_releases()
    }

    fn get(release: &StoredRelease) -> Option<&Arc<Addon<Self>>> {
        match release {
            StoredRelease::Dependency(record) => Some(record),
            _ => None,
        }
    }

    fn manifest(record: Arc<Addon<Self>>) -> StoredRelease {
        StoredRelease::Dependency(record)
    }
}

impl Addons {
    /// Removes an acquired runner release from shared storage.
    ///
    /// Existing environment selections retain their records but lose the payload.
    /// Built Virgo artifacts are unaffected. The release is moved to trash before
    /// the new state is published; trash cleanup is best effort.
    ///
    /// # Errors
    ///
    /// Returns [`AddonError::NotFound`] when the release is absent, or an I/O error
    /// if its storage directory cannot be moved to trash.
    pub async fn remove_runner(&self, id: Uuid) -> Result<()> {
        self.remove::<Runner>(id).await
    }

    /// Removes an acquired winebridge release from shared storage.
    ///
    /// Existing environment selections retain their records but lose the payload.
    /// Built Virgo artifacts are unaffected. The release is moved to trash before
    /// the new state is published; trash cleanup is best effort.
    ///
    /// # Errors
    ///
    /// Returns [`AddonError::NotFound`] when the release is absent, or an I/O error
    /// if its storage directory cannot be moved to trash.
    pub async fn remove_winebridge(&self, id: Uuid) -> Result<()> {
        self.remove::<WineBridge>(id).await
    }

    /// Removes an acquired umu release from shared storage.
    ///
    /// Existing environment selections retain their records but lose the payload.
    /// Built Virgo artifacts are unaffected. The release is moved to trash before
    /// the new state is published; trash cleanup is best effort.
    ///
    /// # Errors
    ///
    /// Returns [`AddonError::NotFound`] when the release is absent, or an I/O error
    /// if its storage directory cannot be moved to trash.
    pub async fn remove_umu(&self, id: Uuid) -> Result<()> {
        self.remove::<Umu>(id).await
    }

    /// Removes an acquired component release from shared storage.
    ///
    /// Existing environment selections retain their records but lose the payload.
    /// Built Virgo artifacts are unaffected. The release is moved to trash before
    /// the new state is published; trash cleanup is best effort.
    ///
    /// # Errors
    ///
    /// Returns [`AddonError::NotFound`] when the release is absent, or an I/O error
    /// if its storage directory cannot be moved to trash.
    pub async fn remove_component(&self, id: Uuid) -> Result<()> {
        self.remove::<Component>(id).await
    }

    /// Removes an acquired dependency release from shared storage.
    ///
    /// Existing environment selections retain their records but lose the payload.
    /// Built Virgo artifacts are unaffected. The release is moved to trash before
    /// the new state is published; trash cleanup is best effort.
    ///
    /// # Errors
    ///
    /// Returns [`AddonError::NotFound`] when the release is absent, or an I/O error
    /// if its storage directory cannot be moved to trash.
    pub async fn remove_dependency(&self, id: Uuid) -> Result<()> {
        self.remove::<Dependency>(id).await
    }

    async fn remove<K: StoredAddon>(&self, id: Uuid) -> Result<()> {
        fs::with_temp_dir(&self.0.directories.trash(), |trash| async move {
            let _write = self.0.write.lock().await;
            let mut next = self.state().as_ref().clone();
            let release = next
                .releases
                .get(&id)
                .and_then(K::get)
                .ok_or(AddonError::NotFound(id))?;
            async_fs::rename(
                release.directory(&self.0.directories),
                trash.join("release"),
            )
            .await?;
            next.releases.remove(&id);
            self.publish(next);
            Ok(())
        })
        .await
    }

    /// Commits a prepared release and publishes the updated snapshot.
    pub(super) async fn commit<K: StoredAddon>(
        &self,
        record: Arc<Addon<K>>,
        prepared: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Arc<Addon<K>>> {
        let id = record.id();
        let destination = record.directory(&self.0.directories);
        let _write = cancellation
            .run_until_cancelled(self.0.write.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut next = self.state().as_ref().clone();
        if let Some(current) = next.releases.get(&id) {
            let current = K::get(current).ok_or(AddonError::Duplicate(id))?;
            if current != &record {
                return Err(AddonError::InvalidRelease(destination).into());
            }
            return Ok(current.clone());
        }
        if crate::utils::fs::exists(&destination).await? {
            return Err(AddonError::TargetExists(destination).into());
        }
        let stored = K::manifest(record.clone());
        next_config::save(prepared.join("release.toml"), &stored).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        async_fs::rename(prepared, destination).await?;
        next.releases.insert(id, stored);
        self.publish(next);
        Ok(record)
    }
}

impl AddonsState {
    /// Loads cached catalogs and release manifests into the initial snapshot.
    pub(super) async fn load_cached(directories: &Directories) -> Result<Self> {
        let mut state = Self {
            component_catalog: Catalog::load(&directories.component_catalog()).await?,
            dependency_catalog: Catalog::load(&directories.dependency_catalog()).await?,
            ..Self::default()
        };
        for root in [
            directories.component_releases(),
            directories.dependency_releases(),
        ] {
            for (id, path) in release_manifests(&root).await? {
                let record: StoredRelease = next_config::load(&path).await?;
                if record.id() != id {
                    return Err(AddonError::InvalidRelease(path).into());
                }
                if state.releases.insert(id, record).is_some() {
                    return Err(AddonError::Duplicate(id).into());
                }
            }
        }
        Ok(state)
    }
}

/// Collects expected manifest paths from UUID-named directories in `root`.
///
/// Non-directory entries and directories without UUID names are ignored.
///
/// # Errors
///
/// Returns an error if the release root or an entry's metadata cannot be read.
async fn release_manifests(root: &Path) -> Result<Vec<(Uuid, PathBuf)>> {
    let mut manifests = Vec::new();
    let mut entries = async_fs::read_dir(root).await?;
    while let Some(entry) = entries.try_next().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        if let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) {
            manifests.push((id, entry.path().join("release.toml")));
        }
    }
    Ok(manifests)
}
