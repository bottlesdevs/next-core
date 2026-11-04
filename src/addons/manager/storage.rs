//! Discovery, atomic commit, and removal of acquired releases.

use super::{Addons, AddonsState};
use crate::{
    Addon, AddonError, AddonRole, Directories, RoleMarker,
    addons::catalog::Catalog,
    error::{Error, Result},
    utils::fs,
};
use futures_util::TryStreamExt;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

impl next_config::Config for Addon<AddonRole> {
    const VERSION: u32 = 1;
}

impl Addons {
    /// Removes an acquired release of any role from shared storage.
    ///
    /// Existing environment selections retain their records but lose the payload.
    /// Built Virgo artifacts are unaffected. The release is moved to trash before
    /// the new state is published; trash cleanup is best effort.
    ///
    /// # Errors
    ///
    /// Returns [`AddonError::NotFound`] when the release is absent, or an I/O error
    /// if its storage directory cannot be moved to trash.
    pub async fn remove(&self, id: Uuid) -> Result<()> {
        fs::with_temp_dir(&self.0.directories.trash(), |trash| async move {
            let _write = self.0.write.lock().await;
            let mut next = self.state().as_ref().clone();
            let release = next.releases.get(&id).ok_or(AddonError::NotFound(id))?;
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
    pub(super) async fn commit<R: RoleMarker>(
        &self,
        record: Addon<R>,
        prepared: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Addon<R>> {
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
            let current = current.clone().cast().ok_or(AddonError::WrongRole(id))?;
            if current != record {
                return Err(AddonError::InvalidRelease(destination).into());
            }
            return Ok(current);
        }
        if crate::utils::fs::exists(&destination).await? {
            return Err(AddonError::TargetExists(destination).into());
        }
        let stored = record.clone().erase();
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
                let record: Addon<AddonRole> = next_config::load(&path).await?;
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
