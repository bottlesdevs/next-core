//! Shared build lifecycle for filesystem and registry effects over pinned Soda.

use std::{
    future::Future,
    path::{Path, PathBuf},
};

use fvs_rs::UnmountMode;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::super::registry;
use super::{VirgoLayer, VirgoManager, cache, remove_dir};
use crate::{
    EnvironmentError,
    environment::{prefix::FVS_BLOCK_SIZE, runtime},
    error::{Error, Result},
    runner::Runner,
};

impl VirgoManager {
    /// The caller holds the build lock. The execution step receives the same runner that
    /// this workflow must stop before diffing and releasing its scratch mount.
    pub(super) async fn build<'a, Fut>(
        &self,
        id: Uuid,
        destination: &Path,
        runner: &'a dyn Runner,
        base: &VirgoLayer,
        cancellation: &CancellationToken,
        work: impl FnOnce(PathBuf, &'a dyn Runner) -> Fut + Send,
    ) -> Result<VirgoLayer>
    where
        Fut: Future<Output = Result<()>> + Send,
    {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let client = self.cx.fvs().await?;
        let stage = self.staging_path();
        let artifact = stage.join("artifact");
        let upper = artifact.join("filesystem");
        let prefix = stage.join("prefix");
        let patches = artifact.join("registry");
        let setup = async {
            async_fs::create_dir_all(&upper).await?;
            async_fs::create_dir_all(&prefix).await?;
            Ok::<_, Error>(())
        }
        .await;
        if let Err(error) = setup {
            remove_dir(stage).await;
            return Err(error);
        }
        let mount = client
            .mount(&prefix, vec![base.layer.clone()], Some(&upper))
            .await?;
        let executed = async {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            work(prefix.clone(), runner).await
        }
        .await;
        // Stop even if execution failed; retain storage when shutdown fails.
        runtime::stop(runner, &prefix).await?;
        let diffed = async {
            executed?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            registry::write_patches(&base.registry, &prefix, &patches).await?;
            client.diff_mount(&mount, true).await?;
            Ok::<_, Error>(())
        }
        .await;
        client
            .unmount(&mount, UnmountMode::Normal)
            .await
            .map_err(|source| EnvironmentError::Cleanup {
                prefix: prefix.clone(),
                source: Box::new(source.into()),
            })?;
        let result = async {
            diffed?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            registry::exclude_hives(&upper).await?;
            let repository = client.new_repository(&upper, FVS_BLOCK_SIZE).await?;
            let commit = client.commit(&repository, id.to_string()).await?;
            cache::publish(&artifact, destination, id, commit.state_id, cancellation).await
        }
        .await;
        remove_dir(stage).await;
        result
    }
}
