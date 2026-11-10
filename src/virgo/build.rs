//! Builds immutable filesystem and Wine registry artifacts in isolated staging.
//!
//! A build may start from an empty filesystem or an FVS mount over an existing
//! base layer. Execution and Wine shutdown are completed by the caller before
//! [`LayerStore::finish_build`] captures registry effects and publishes the result.

use std::{
    future::Future,
    path::{Path, PathBuf},
};

use fvs_rs::{Mount, UnmountMode};
use uuid::Uuid;

use super::{FVS_BLOCK_SIZE, LayerStore, VirgoLayer, registry};
use crate::{
    Job,
    error::{Error, Result, ResultExt},
};

/// Holds staging paths and an optional base mount for one artifact build.
///
/// Dropping this value performs no cleanup. Callers must stop every process and
/// pass the workspace to [`LayerStore::finish_build`] so mount release and
/// publication occur in the required order.
pub(crate) struct BuildWorkspace {
    destination: PathBuf,
    stage: PathBuf,
    /// Prefix in which the build recipe executes.
    pub(crate) wineprefix: PathBuf,
    overlay: Option<(Mount, PathBuf)>,
}

impl LayerStore {
    /// Returns a cached artifact or serializes one cache-miss build.
    ///
    /// The cache is checked before and after acquiring the global build lock. The
    /// `build` callback therefore runs only when the artifact is still absent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cancelled`], cache loading errors, or the callback's error.
    pub(crate) async fn get_or_build<Fut>(
        &self,
        key: &Path,
        id: Option<Uuid>,
        job: &Job,
        build: impl FnOnce() -> Fut,
    ) -> Result<VirgoLayer>
    where
        Fut: Future<Output = Result<VirgoLayer>>,
    {
        if let Some(layer) = self.load(key, id).await? {
            return Ok(layer);
        }
        let _lock = job.cancellable(self.build_lock.lock()).await?;
        if let Some(layer) = self.load(key, id).await? {
            return Ok(layer);
        }
        build().await
    }

    /// Creates isolated staging for a build, optionally mounted over `base`.
    ///
    /// This must run inside [`Self::get_or_build`] after all inputs are resolved
    /// and before any process starts. Cancellation is sampled before setup but
    /// does not interrupt an active FVS mount request.
    ///
    /// # Errors
    ///
    /// Returns cancellation, filesystem, or FVS mount errors. Failed directory
    /// setup is removed best effort; a failed mount request retains staging because
    /// the mount outcome may be uncertain.
    pub(crate) async fn prepare_build(
        &self,
        key: &Path,
        base: Option<&VirgoLayer>,
        job: &Job,
    ) -> Result<BuildWorkspace> {
        job.check_cancellation()?;
        let stage = self.staging_path();
        let filesystem = stage.join("artifact/filesystem");
        let wineprefix = if base.is_some() {
            stage.join("prefix")
        } else {
            filesystem.clone()
        };
        let setup = async {
            async_fs::create_dir_all(&filesystem).await?;
            if base.is_some() {
                async_fs::create_dir_all(&wineprefix).await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        if let Err(error) = setup {
            async_fs::remove_dir_all(stage).await.log_warn();
            return Err(error);
        }
        // A failed mount request retains staging because its outcome may be uncertain.
        let overlay = if let Some(base) = base {
            let mount = self
                .fvs
                .mount(&wineprefix, vec![base.layer.clone()], Some(&filesystem))
                .await?;
            Some((mount, base.registry.clone()))
        } else {
            None
        };
        Ok(BuildWorkspace {
            destination: key.to_path_buf(),
            stage,
            wineprefix,
            overlay,
        })
    }

    /// Captures, unmounts, commits, and publishes a completed build.
    ///
    /// The caller must stop build processes before calling this method inside
    /// [`Self::get_or_build`]. A failed recipe is discarded after a successful
    /// unmount; an unmount failure retains the workspace and returns immediately.
    /// Cancellation is sampled before commit; it does not interrupt an active FVS
    /// request.
    ///
    /// # Errors
    ///
    /// Returns the recipe error, cancellation, registry processing, FVS, filesystem,
    /// or publication errors. Staging cleanup after a settled unmount is best effort.
    pub(crate) async fn finish_build(
        &self,
        workspace: BuildWorkspace,
        id: Uuid,
        message: String,
        executed: Result<()>,
        job: &Job,
    ) -> Result<VirgoLayer> {
        let artifact = workspace.stage.join("artifact");
        let filesystem = artifact.join("filesystem");
        let registry = artifact.join("registry");
        let captured = async {
            executed?;
            if let Some((mount, base)) = &workspace.overlay {
                registry::write_patches(base, &workspace.wineprefix, &registry).await?;
                self.fvs.diff_mount(mount, true).await?;
            } else {
                registry::capture(&filesystem, &registry).await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        if let Some((mount, _)) = &workspace.overlay {
            self.fvs.unmount(mount, UnmountMode::Normal).await?;
        }
        let result = async {
            captured?;
            job.check_cancellation()?;
            if workspace.overlay.is_some() {
                registry::exclude_hives(&filesystem).await?;
            }
            let repository = self.fvs.new_repository(&filesystem, FVS_BLOCK_SIZE).await?;
            let commit = self.fvs.commit(&repository, message).await?;
            self.publish(&artifact, &workspace.destination, id, commit.state_id)
                .await
        }
        .await;
        async_fs::remove_dir_all(workspace.stage).await.log_warn();
        result
    }
}
