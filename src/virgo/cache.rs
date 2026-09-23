//! Loads and publishes immutable Virgo artifacts.
//!
//! Each published directory contains a manifest, an FVS-backed `filesystem`
//! repository, and registry data. Absence is the only cache miss; malformed or
//! mismatched entries fail rather than being silently rebuilt over.

use std::path::{Path, PathBuf};

use futures_lite::StreamExt;
use fvs_rs::{Layer, Repository};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{LayerStore, VirgoError};
use crate::{
    error::{Error, Result},
    utils::fs,
};

#[derive(Deserialize, Serialize, next_config::Config)]
#[config(version = 1)]
struct VirgoLayerManifest {
    id: Uuid,
    commit: String,
}

/// Resolves an artifact's filesystem layer and registry effects.
///
/// The artifact kind and original build inputs are encoded by its cache key,
/// outside this value.
pub(crate) struct VirgoLayer {
    /// Immutable addon or component identifier recorded by the manifest.
    pub(crate) id: Uuid,
    /// FVS filesystem revision used during composition.
    pub(crate) layer: Layer,
    /// Directory containing the artifact's registry baseline or patches.
    pub(crate) registry: PathBuf,
}

impl VirgoLayerManifest {
    fn resolve(self, root: &Path) -> VirgoLayer {
        let repository = Repository {
            repository_path: root.join("filesystem").display().to_string(),
            block_size: 0,
        };
        VirgoLayer {
            id: self.id,
            layer: Layer::from_state_id(&repository, Some(&self.commit)),
            registry: root.join("registry"),
        }
    }
}

impl LayerStore {
    /// Loads a published artifact, treating only an absent directory as a cache miss.
    ///
    /// When `id` is supplied, the manifest must record that exact identifier. The
    /// filesystem and registry contents remain lazily validated by their consumers.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem or manifest failures, an identity mismatch,
    /// or an empty FVS commit identifier.
    pub(crate) async fn load(&self, key: &Path, id: Option<Uuid>) -> Result<Option<VirgoLayer>> {
        let root = self.directories.virgo().join(key);
        match async_fs::symlink_metadata(&root).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let manifest: VirgoLayerManifest = next_config::load(root.join("manifest.toml")).await?;
        if id.is_some_and(|id| manifest.id != id) || manifest.commit.is_empty() {
            return Err(VirgoError::InvalidArtifact(root.to_path_buf()).into());
        }
        Ok(Some(manifest.resolve(&root)))
    }

    /// Loads an artifact and rejects a cache miss.
    ///
    /// # Errors
    ///
    /// Returns [`VirgoError::MissingArtifact`] when `key` is absent, plus every
    /// error described by [`Self::load`].
    pub(crate) async fn require(&self, key: &Path, id: Option<Uuid>) -> Result<VirgoLayer> {
        self.load(key, id)
            .await?
            .ok_or_else(|| VirgoError::MissingArtifact(self.directories.virgo().join(key)).into())
    }

    /// Lists immediate published children of a collection without FVS requests.
    ///
    /// A manifest identifies an artifact. Staging, files, and directories without
    /// a manifest are skipped; results are sorted by relative key.
    ///
    /// # Errors
    ///
    /// Returns an error while reading the collection or any discovered manifest.
    #[allow(dead_code)] // Internal storage API; no public owner-facing layer API.
    pub(crate) async fn list(&self, collection: &Path) -> Result<Vec<(PathBuf, VirgoLayer)>> {
        let mut entries = match async_fs::read_dir(self.directories.virgo().join(collection)).await
        {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut layers = Vec::new();
        while let Some(entry) = entries.try_next().await? {
            if entry.file_name() == ".staging" || !entry.file_type().await?.is_dir() {
                continue;
            }
            if crate::utils::fs::exists(&entry.path().join("manifest.toml")).await? {
                let key = collection.join(entry.file_name());
                layers.push((key.clone(), self.require(&key, None).await?));
            }
        }
        layers.sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(layers)
    }

    /// Withdraws an explicitly addressed artifact into trash.
    ///
    /// The caller must ensure it is unmounted and unused by every workspace.
    /// Cancellation is honored before the rename; cleanup afterward is best effort.
    ///
    /// # Errors
    ///
    /// Returns cancellation or filesystem errors before withdrawal completes.
    #[allow(dead_code)] // Internal storage API; callers own reference tracking.
    pub(crate) async fn remove(&self, key: &Path, cancellation: &CancellationToken) -> Result<()> {
        let _lock = cancellation
            .run_until_cancelled(self.build_lock.lock())
            .await
            .ok_or(Error::Cancelled)?;
        fs::with_temp_dir(&self.directories.trash(), |trash| async move {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            async_fs::rename(self.directories.virgo().join(key), trash.join("artifact")).await?;
            Ok(())
        })
        .await
    }

    /// Publishes a completed staged artifact at its immutable cache key.
    ///
    /// The caller must hold the build lock and provide a committed filesystem plus
    /// both registry files. Existing destination directories are never replaced.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be saved, the destination parent
    /// cannot be created, cancellation arrives before the rename, or publication fails.
    pub(super) async fn publish(
        &self,
        artifact: &Path,
        key: &Path,
        id: Uuid,
        commit: String,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let destination = self.directories.virgo().join(key);
        let manifest = VirgoLayerManifest { id, commit };
        next_config::save(artifact.join("manifest.toml"), &manifest).await?;
        async_fs::create_dir_all(destination.parent().expect("artifact has a parent")).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        async_fs::rename(artifact, &destination).await?;
        Ok(manifest.resolve(&destination))
    }
}
