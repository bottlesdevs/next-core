//! Coordinated construction of immutable filesystem and Wine registry artifacts.

use std::{
    future::Future,
    path::{Path, PathBuf},
};

use fvs_rs::{Mount, UnmountMode};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{FVS_BLOCK_SIZE, LayerStore, VirgoError, VirgoLayer, registry};
use crate::error::{Error, Result};

/// Prepared storage only. Dropping retains it; finalization requires stopped processes.
pub(crate) struct BuildWorkspace {
    destination: PathBuf,
    stage: PathBuf,
    pub(crate) prefix: PathBuf,
    overlay: Option<(Mount, PathBuf)>,
}

impl LayerStore {
    /// Resolve inputs and complete construction inside this scope, only after a cache miss.
    pub(crate) async fn get_or_build<Fut>(
        &self,
        key: &Path,
        id: Option<Uuid>,
        cancellation: &CancellationToken,
        build: impl FnOnce() -> Fut,
    ) -> Result<VirgoLayer>
    where
        Fut: Future<Output = Result<VirgoLayer>>,
    {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if let Some(layer) = self.load(key, id).await? {
            return Ok(layer);
        }
        let _lock = cancellation
            .run_until_cancelled(self.build_lock.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if let Some(layer) = self.load(key, id).await? {
            return Ok(layer);
        }
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        build().await
    }

    /// Prepare inside get_or_build, after resolving inputs and before starting processes.
    pub(crate) async fn prepare_build(
        &self,
        key: &Path,
        base: Option<&VirgoLayer>,
        cancellation: &CancellationToken,
    ) -> Result<BuildWorkspace> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let stage = self.staging_path();
        let filesystem = stage.join("artifact/filesystem");
        let prefix = if base.is_some() {
            stage.join("prefix")
        } else {
            filesystem.clone()
        };
        let setup = async {
            async_fs::create_dir_all(&filesystem).await?;
            if base.is_some() {
                async_fs::create_dir_all(&prefix).await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        if let Err(error) = setup {
            let _ = async_fs::remove_dir_all(&stage).await;
            return Err(error);
        }
        // A failed mount request retains staging because its outcome may be uncertain.
        let overlay = if let Some(base) = base {
            let mount = self
                .fvs
                .mount(&prefix, vec![base.layer.clone()], Some(&filesystem))
                .await?;
            Some((mount, base.registry.clone()))
        } else {
            None
        };
        Ok(BuildWorkspace {
            destination: key.to_path_buf(),
            stage,
            prefix,
            overlay,
        })
    }

    /// Consume the execution result only after shutdown succeeds, within get_or_build.
    /// Failed execution is discarded; failed unmount retains the workspace.
    pub(crate) async fn finish_build(
        &self,
        workspace: BuildWorkspace,
        id: Uuid,
        message: String,
        executed: Result<()>,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let artifact = workspace.stage.join("artifact");
        let filesystem = artifact.join("filesystem");
        let registry = artifact.join("registry");
        let captured = async {
            executed?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if let Some((mount, base)) = &workspace.overlay {
                registry::write_patches(base, &workspace.prefix, &registry).await?;
                self.fvs.diff_mount(mount, true).await?;
            } else {
                registry::capture(&filesystem, &registry).await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        if let Some((mount, _)) = &workspace.overlay {
            self.fvs
                .unmount(mount, UnmountMode::Normal)
                .await
                .map_err(|source| VirgoError::Unmount {
                    path: workspace.prefix.clone(),
                    source,
                })?;
        }
        let result = async {
            captured?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if workspace.overlay.is_some() {
                registry::exclude_hives(&filesystem).await?;
            }
            let repository = self.fvs.new_repository(&filesystem, FVS_BLOCK_SIZE).await?;
            let commit = self.fvs.commit(&repository, message).await?;
            self.publish(
                &artifact,
                &workspace.destination,
                id,
                commit.state_id,
                cancellation,
            )
            .await
        }
        .await;
        async_fs::remove_dir_all(workspace.stage).await?;
        result
    }
}
