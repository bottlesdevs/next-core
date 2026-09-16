//! Compose immutable layers with a workspace's private filesystem and registry changes.

use std::path::Path;

use futures_lite::StreamExt;
use fvs_rs::UnmountMode;
use tokio_util::sync::CancellationToken;

use super::{LayerStore, VirgoError, VirgoLayer, registry};
use crate::error::{Error, Result};

impl LayerStore {
    /// The caller has stopped processes, unmounted, and checkpointed this workspace.
    /// Overlay order is used for both filesystem and registry precedence.
    /// On failure, unmount before restoring the caller's checkpoint.
    pub(crate) async fn compose_and_mount(
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
        async_fs::create_dir_all(&upper).await?;
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
