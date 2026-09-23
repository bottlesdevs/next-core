//! Local release discovery, commit, and removal.

use super::{Addons, AddonsState};
use crate::{
    Addon, AddonError, Component, Dependency, Directories,
    addons::catalog::Catalog,
    error::{Error, Result},
    utils::fs,
};
use futures_util::TryStreamExt;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

impl Addons {
    /// Withdraws a component's release directory, then cleans it up best effort.
    /// Built Virgo artifacts and environment selections remain unchanged.
    pub async fn remove_component(&self, id: Uuid) -> Result<()> {
        fs::with_temp_dir(&self.0.directories.trash(), |trash| async move {
            let _write = self.0.write.lock().await;
            let mut next = self.state().as_ref().clone();
            let release = next
                .components
                .remove(&id)
                .ok_or(AddonError::NotFound(id))?;
            async_fs::rename(
                release.directory(&self.0.directories),
                trash.join("release"),
            )
            .await?;
            self.publish(next);
            Ok(())
        })
        .await
    }

    /// Withdraws a dependency's release directory, then cleans it up best effort.
    /// Built Virgo artifacts and environment selections remain unchanged.
    pub async fn remove_dependency(&self, id: Uuid) -> Result<()> {
        fs::with_temp_dir(&self.0.directories.trash(), |trash| async move {
            let _write = self.0.write.lock().await;
            let mut next = self.state().as_ref().clone();
            let release = next
                .dependencies
                .remove(&id)
                .ok_or(AddonError::NotFound(id))?;
            async_fs::rename(
                release.directory(&self.0.directories),
                trash.join("release"),
            )
            .await?;
            self.publish(next);
            Ok(())
        })
        .await
    }

    pub(super) async fn commit_component(
        &self,
        record: Arc<Addon<Component>>,
        prepared: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Arc<Addon<Component>>> {
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
        if let Some(current) = next.components.get(&id) {
            if current != &record {
                return Err(AddonError::InvalidRelease(destination).into());
            }
            return Ok(current.clone());
        }
        if next.contains(id) {
            return Err(AddonError::Duplicate(id).into());
        }
        if crate::utils::fs::exists(&destination).await? {
            return Err(AddonError::TargetExists(destination).into());
        }
        next_config::save(prepared.join("release.toml"), record.as_ref()).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        async_fs::rename(prepared, destination).await?;
        next.components.insert(id, record.clone());
        self.publish(next);
        Ok(record)
    }

    pub(super) async fn commit_dependency(
        &self,
        record: Arc<Addon<Dependency>>,
        prepared: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Arc<Addon<Dependency>>> {
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
        if let Some(current) = next.dependencies.get(&id) {
            if current != &record {
                return Err(AddonError::InvalidRelease(destination).into());
            }
            return Ok(current.clone());
        }
        if next.contains(id) {
            return Err(AddonError::Duplicate(id).into());
        }
        if crate::utils::fs::exists(&destination).await? {
            return Err(AddonError::TargetExists(destination).into());
        }
        next_config::save(prepared.join("release.toml"), record.as_ref()).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        async_fs::rename(prepared, destination).await?;
        next.dependencies.insert(id, record.clone());
        self.publish(next);
        Ok(record)
    }
}

impl AddonsState {
    pub(super) async fn load_cached(directories: &Directories) -> Result<Self> {
        let mut state = Self {
            component_catalog: Catalog::<Component>::load(directories).await?,
            dependency_catalog: Catalog::<Dependency>::load(directories).await?,
            ..Self::default()
        };
        for (id, path) in release_manifests(&directories.component_releases()).await? {
            let record: Addon<Component> = next_config::load(&path).await?;
            if record.id() != id {
                return Err(AddonError::InvalidRelease(path).into());
            }
            if state.contains(id) {
                return Err(AddonError::Duplicate(id).into());
            }
            state.components.insert(id, Arc::new(record));
        }
        for (id, path) in release_manifests(&directories.dependency_releases()).await? {
            let record: Addon<Dependency> = next_config::load(&path).await?;
            if record.id() != id {
                return Err(AddonError::InvalidRelease(path).into());
            }
            if state.contains(id) {
                return Err(AddonError::Duplicate(id).into());
            }
            state.dependencies.insert(id, Arc::new(record));
        }
        Ok(state)
    }
}

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
