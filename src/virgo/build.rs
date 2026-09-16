//! Coordinated construction of immutable filesystem and Wine registry artifacts.

use std::path::{Path, PathBuf};

use fvs_rs::{Mount, UnmountMode};
use tokio::sync::MutexGuard;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{FVS_BLOCK_SIZE, LayerStore, VirgoError, VirgoLayer, registry};
use crate::error::{Error, Result};

pub(crate) enum Reservation<'a> {
    Cached(VirgoLayer),
    Build(LayerBuild<'a>),
}

/// Holds publication coordination until explicitly finished, discarded, or dropped.
/// Dropping retains storage and mounts: callers must first stop processes using them.
pub(crate) struct LayerBuild<'a> {
    store: &'a LayerStore,
    _lock: MutexGuard<'a, ()>,
    key: PathBuf,
    stage: PathBuf,
    base_registry: Option<PathBuf>,
    mount: Option<Mount>,
}

impl LayerStore {
    /// Reserve before resolving source inputs. Cache hits need only published metadata.
    pub(crate) async fn reserve(
        &self,
        key: &Path,
        id: Option<Uuid>,
        cancellation: &CancellationToken,
    ) -> Result<Reservation<'_>> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if let Some(layer) = self.load(key, id).await? {
            return Ok(Reservation::Cached(layer));
        }
        let lock = cancellation
            .run_until_cancelled(self.build_lock.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if let Some(layer) = self.load(key, id).await? {
            return Ok(Reservation::Cached(layer));
        }
        Ok(Reservation::Build(LayerBuild {
            store: self,
            _lock: lock,
            key: key.to_path_buf(),
            stage: self.staging_path(),
            base_registry: None,
            mount: None,
        }))
    }
}

impl LayerBuild<'_> {
    /// Prepare either a standalone base or a writable overlay over one published layer.
    /// No processes may use the returned workspace until preparation succeeds.
    pub(crate) async fn prepare(
        &mut self,
        base: Option<&VirgoLayer>,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let filesystem = self.stage.join("artifact/filesystem");
        async_fs::create_dir_all(&filesystem).await?;
        if let Some(base) = base {
            self.base_registry = Some(base.registry.clone());
            let prefix = self.stage.join("prefix");
            async_fs::create_dir_all(&prefix).await?;
            self.mount = Some(
                self.store
                    .fvs
                    .mount(&prefix, vec![base.layer.clone()], Some(&filesystem))
                    .await?,
            );
            Ok(prefix)
        } else {
            Ok(filesystem)
        }
    }

    /// Capture and publish only after execution succeeded and its processes were stopped.
    /// Failures release mounts before cleanup; a failed unmount retains the workspace.
    pub(crate) async fn finish(
        mut self,
        id: Uuid,
        message: String,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let artifact = self.stage.join("artifact");
        let filesystem = artifact.join("filesystem");
        let registry = artifact.join("registry");
        let captured = async {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if let Some(base) = &self.base_registry {
                registry::write_patches(base, &self.stage.join("prefix"), &registry).await?;
                self.store
                    .fvs
                    .diff_mount(self.mount.as_ref().expect("prepared overlay"), true)
                    .await?;
            } else {
                registry::capture(&filesystem, &registry).await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        self.unmount().await?;
        let result = async {
            captured?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if self.base_registry.is_some() {
                registry::exclude_hives(&filesystem).await?;
            }
            let repository = self
                .store
                .fvs
                .new_repository(&filesystem, FVS_BLOCK_SIZE)
                .await?;
            let commit = self.store.fvs.commit(&repository, message).await?;
            self.store
                .publish(&artifact, &self.key, id, commit.state_id, cancellation)
                .await
        }
        .await;
        self.discard().await?;
        result
    }

    /// Call only before execution starts or after its processes have stopped successfully.
    pub(crate) async fn discard(mut self) -> Result<()> {
        self.unmount().await?;
        match async_fs::remove_dir_all(&self.stage).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn unmount(&mut self) -> Result<()> {
        if let Some(mount) = &self.mount {
            self.store
                .fvs
                .unmount(mount, UnmountMode::Normal)
                .await
                .map_err(|source| VirgoError::Unmount {
                    path: self.stage.join("prefix"),
                    source,
                })?;
            self.mount = None;
        }
        Ok(())
    }
}
