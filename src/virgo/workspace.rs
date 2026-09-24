//! Prepares private registry state and mounts a Virgo workspace for execution.
//!
//! Registry composition occurs only while the environment is stopped and
//! unmounted. Runtime startup then mounts the selected filesystem layers over the
//! private upper directory without changing the composed registry files.

use std::path::Path;

use futures_lite::StreamExt;
use fvs_rs::UnmountMode;
use tokio_util::sync::CancellationToken;

use super::{LayerStore, VirgoError, VirgoLayer, registry};
use crate::error::{Error, Result};

impl LayerStore {
    /// Composes selected registry effects into a stopped, unmounted workspace.
    ///
    /// The caller is responsible for checkpointing an existing owner and recovering
    /// failures before publishing a new selection. Cancellation is sampled before
    /// and after composition but does not interrupt the blocking registry work.
    ///
    /// # Errors
    ///
    /// Returns cancellation, filesystem, registry parsing, or patch application errors.
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
            &self.directories.staging(),
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

    /// Mounts selected filesystem layers over the prepared private upper directory.
    ///
    /// Registry files are not recomposed here. The caller must release storage if
    /// mounting fails or cancellation arrives after FVS creates the mount.
    ///
    /// # Errors
    ///
    /// Returns [`VirgoError::DirtyMountpoint`] when the prefix contains files,
    /// [`Error::Cancelled`], or a filesystem/FVS error.
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

    /// Releases this workspace's active mount after its processes have stopped.
    ///
    /// An absent prefix or unregistered mount is a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if prefix inspection, mount discovery, or unmounting fails.
    pub(crate) async fn unmount_workspace(&self, root: &Path) -> Result<()> {
        let prefix = root.join("prefix");
        if crate::utils::fs::exists(&prefix).await? {
            if let Some(mount) = self.fvs.list_mounts().await?.into_iter().find(|mount| {
                mount
                    .spec
                    .as_ref()
                    .is_some_and(|spec| spec.mount_point == prefix.to_string_lossy())
            }) {
                self.fvs.unmount(&mount, UnmountMode::Normal).await?;
            }
        }
        Ok(())
    }
}

/// Creates an empty mountpoint or rejects contents that mounting would hide.
///
/// # Errors
///
/// Returns [`VirgoError::DirtyMountpoint`] for a nonempty directory, or an I/O
/// error while creating or reading it.
async fn ensure_empty_dir(path: &Path) -> Result<()> {
    async_fs::create_dir_all(path).await?;
    if async_fs::read_dir(path).await?.try_next().await?.is_some() {
        return Err(VirgoError::DirtyMountpoint(path.to_path_buf()).into());
    }
    Ok(())
}
