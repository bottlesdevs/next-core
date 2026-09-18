//! Storage for complete immutable bases, addons, and runner adapters.

use std::path::{Path, PathBuf};

use futures_lite::StreamExt;
use fvs_rs::{Layer, Repository};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{LayerStore, VirgoError, registry::registry_files};
use crate::{
    error::{Error, Result},
    utils::storage,
};

#[derive(Deserialize, Serialize, next_config::Config)]
#[config(version = 1)]
struct VirgoLayerManifest {
    id: Uuid,
    commit: String,
}

/// Resolved installed effects, independent of build inputs and artifact kind.
pub(crate) struct VirgoLayer {
    pub(crate) id: Uuid,
    pub(crate) layer: Layer,
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
    /// Only an absent directory is a cache miss.
    /// UUID-keyed caches check the expected ID; the fixed base directory discovers its pinned ID.
    pub(crate) async fn load(&self, key: &Path, id: Option<Uuid>) -> Result<Option<VirgoLayer>> {
        let root = self.directories.virgo().join(key);
        match async_fs::symlink_metadata(&root).await {
            Ok(entry) if entry.is_dir() => {}
            Ok(_) => return Err(VirgoError::InvalidArtifact(root.to_path_buf()).into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let manifest: VirgoLayerManifest = next_config::load(root.join("manifest.toml")).await?;
        if id.is_some_and(|id| manifest.id != id) || manifest.commit.is_empty() {
            return Err(VirgoError::InvalidArtifact(root.to_path_buf()).into());
        }
        if !async_fs::metadata(root.join("filesystem/.fvs2"))
            .await?
            .is_dir()
        {
            return Err(VirgoError::InvalidArtifact(root.to_path_buf()).into());
        }
        for (file, _) in registry_files() {
            if !async_fs::metadata(root.join("registry").join(file))
                .await?
                .is_file()
            {
                return Err(VirgoError::InvalidArtifact(root.to_path_buf()).into());
            }
        }
        Ok(Some(manifest.resolve(&root)))
    }

    pub(crate) async fn require(&self, key: &Path, id: Option<Uuid>) -> Result<VirgoLayer> {
        self.load(key, id)
            .await?
            .ok_or_else(|| VirgoError::MissingArtifact(self.directories.virgo().join(key)).into())
    }

    /// List immediate published children of a relative collection, without FVS RPCs.
    /// A manifest identifies an artifact; staging and other entries are skipped.
    /// Malformed manifests or incomplete published artifacts fail the listing.
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
            if crate::utils::exists(&entry.path().join("manifest.toml")).await? {
                let key = collection.join(entry.file_name());
                layers.push((key.clone(), self.require(&key, None).await?));
            }
        }
        layers.sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(layers)
    }

    /// Withdraw an explicitly addressed artifact into trash, then clean up best effort.
    /// The caller must ensure it is unmounted and no longer needed by any workspace.
    /// Cancellation is honored before withdrawal; cleanup cannot invalidate removal.
    #[allow(dead_code)] // Internal storage API; callers own reference tracking.
    pub(crate) async fn remove(&self, key: &Path, cancellation: &CancellationToken) -> Result<()> {
        let _lock = cancellation
            .run_until_cancelled(self.build_lock.lock())
            .await
            .ok_or(Error::Cancelled)?;
        storage::with_temp_dir(&self.directories.trash(), |trash| async move {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            async_fs::rename(self.directories.virgo().join(key), trash.join("artifact")).await?;
            Ok(())
        })
        .await
    }

    /// The caller has committed the filesystem and written both registry files in staging.
    /// The shared build lock covers publication; published nonempty directories are never replaced.
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
