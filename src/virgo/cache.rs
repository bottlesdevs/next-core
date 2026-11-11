//! Loads and publishes immutable Virgo artifacts.
//!
//! Each published directory contains a manifest, an FVS-backed `filesystem`
//! repository, and registry data. Absence is the only cache miss; malformed or
//! mismatched entries fail rather than being silently rebuilt over.

use std::path::{Path, PathBuf};

use fvs_rs::{Layer, Repository};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{LayerStore, VirgoError};
use crate::error::Result;

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
        let root = self.root().join(key);
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
            .ok_or_else(|| VirgoError::MissingArtifact(self.root().join(key)).into())
    }

    /// Publishes a completed staged artifact at its immutable cache key.
    ///
    /// The caller must hold the build lock and provide a committed filesystem plus
    /// both registry files. The destination must remain absent until the final
    /// same-filesystem rename; this function does not independently enforce that
    /// invariant.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be saved, the destination parent
    /// cannot be created, or publication fails.
    ///
    /// # Panics
    ///
    /// Panics if joining `key` to the Virgo root produces a path without a parent.
    pub(super) async fn publish(
        &self,
        artifact: &Path,
        key: &Path,
        id: Uuid,
        commit: String,
    ) -> Result<VirgoLayer> {
        let destination = self.root().join(key);
        let manifest = VirgoLayerManifest { id, commit };
        next_config::save(artifact.join("manifest.toml"), &manifest).await?;
        async_fs::create_dir_all(destination.parent().expect("artifact has a parent")).await?;
        async_fs::rename(artifact, &destination).await?;
        tracing::info!(layer = %key.display(), "published Virgo layer");
        Ok(manifest.resolve(&destination))
    }
}
