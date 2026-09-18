//! Prepare a workspace's registry on selection changes; mount it for execution.

use std::path::Path;

use futures_lite::StreamExt;
use fvs_rs::UnmountMode;
use tokio_util::sync::CancellationToken;

use super::{LayerStore, VirgoError, VirgoLayer, registry};
use crate::error::{Error, Result};

impl LayerStore {
    /// Compose registry files while stopped and unmounted. The caller checkpoints
    /// existing workspaces and recovers failures before publishing new selections.
    pub(crate) async fn prepare_workspace(
        &self,
        root: &Path,
        base: &VirgoLayer,
        overlays: &[VirgoLayer],
        cancellation: &CancellationToken,
    ) -> Result<()> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        async_fs::create_dir_all(root.join("upper")).await?;
        registry::compose(
            root,
            &base.registry,
            overlays
                .iter()
                .map(|layer| layer.registry.clone())
                .collect(),
        )
        .await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    /// Mount the saved selection over its prepared private storage without changing
    /// registry files. The caller releases any mount if mounting fails or is cancelled.
    pub(crate) async fn mount_workspace(
        &self,
        root: &Path,
        base: &VirgoLayer,
        overlays: &[VirgoLayer],
        cancellation: &CancellationToken,
    ) -> Result<()> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let prefix = root.join("prefix");
        ensure_empty_dir(&prefix).await?;
        let upper = root.join("upper");
        let layers = std::iter::once(base.layer.clone())
            .chain(overlays.iter().map(|layer| layer.layer.clone()))
            .collect();
        self.fvs.mount(&prefix, layers, Some(&upper)).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    /// Release the workspace mount after its processes have stopped.
    pub(crate) async fn unmount_workspace(&self, root: &Path) -> Result<()> {
        let prefix = root.join("prefix");
        if crate::utils::exists(&prefix).await? {
            if let Some(mount) = self.fvs.list_mounts().await?.into_iter().find(|mount| {
                mount
                    .spec
                    .as_ref()
                    .is_some_and(|spec| spec.mount_point == prefix.to_string_lossy())
            }) {
                self.fvs
                    .unmount(&mount, UnmountMode::Normal)
                    .await
                    .map_err(|source| VirgoError::Unmount {
                        path: prefix,
                        source,
                    })?;
            }
        }
        Ok(())
    }
}

/// Refuse to mount over existing contents, which would otherwise be hidden.
async fn ensure_empty_dir(path: &Path) -> Result<()> {
    async_fs::create_dir_all(path).await?;
    if async_fs::read_dir(path).await?.try_next().await?.is_some() {
        return Err(VirgoError::DirtyMountpoint(path.to_path_buf()).into());
    }
    Ok(())
}
