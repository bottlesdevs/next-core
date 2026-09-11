//! Layered Virgo prefix storage.
//!
//! A mounted prefix combines a shared base, a runner-specific adapter, cached
//! addon layers, and the owner's writable `upper` directory. Layer order is
//! derived from selected addons when preparing a stopped environment.

mod artifacts;
mod registry;

use std::path::{Path, PathBuf};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use futures_lite::StreamExt;
use fvs_rs::{Layer, UnmountMode};

use crate::{
    Context, Progress, Stage,
    environment::{EnvironmentConfig, history},
    error::{Error, Result},
};

/// Virgo-specific failures carried by [`crate::error::Error::Virgo`].
#[derive(Debug, thiserror::Error)]
pub enum VirgoError {
    #[error("no Soda runner release in the current component catalog")]
    SodaNotInCatalog,
    #[error("invalid Soda semantic version: {0}")]
    InvalidSodaVersion(String),
    #[error("download Soda {version} ({id}) before building the Virgo base or an addon layer")]
    SodaNotDownloaded { id: Uuid, version: String },
    #[error("cyclic addon prerequisites involving {0}")]
    CyclicPrerequisites(Uuid),

    /// A required FVS commit is missing from a repository.
    #[error("FVS repository {repository} has no commit {state}")]
    MissingCommit {
        /// Repository whose history was searched.
        repository: PathBuf,
        /// Requested full or abbreviated state ID.
        state: String,
    },
    /// Virgo cannot mount a prefix over a nonempty mountpoint.
    #[error("mountpoint is not empty: {0}")]
    DirtyMountpoint(PathBuf),
    #[error(
        "mounted layers or writable upper differ from the selected composition at {0}; call stop() and retry"
    )]
    MountMismatch(PathBuf),
    /// A cached layer required to construct the prefix is missing.
    #[error("cached Virgo layer was not found: {0}")]
    CachedLayerNotFound(PathBuf),
    /// Registry data could not be converted while building a Virgo layer.
    #[error("failed to process Virgo registry data: {0}")]
    Registry(String),
}

/// Builds the selected Virgo composition while the owner is coordinated and stopped.
pub(super) async fn prepare(
    config: &EnvironmentConfig,
    root: &Path,
    cx: &Context,
    addons: &crate::Addons,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    let ids: Vec<_> = config
        .ordered_components()
        .map(crate::Addon::id)
        .chain(config.dependencies.iter().map(crate::Addon::id))
        .collect();
    let runner = config
        .runner()
        .load_runner(cx.directories(), config.umu())
        .await?;
    for id in &ids {
        artifacts::prepare_addon(*id, config, addons, cx, progress, cancellation).await?;
    }
    let mut layers = artifacts::base_layers(
        runner.as_ref(),
        &config.runner().id().to_string(),
        addons,
        cx,
        cancellation,
    )
    .await?;
    for id in &ids {
        layers.push(artifacts::cache::layer(*id, cx).await?);
    }
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let checkpoint = history::capture(
        root,
        history::AUTO_CHECKPOINT_MESSAGE.into(),
        false,
        Stage::Checkpointing,
        cx,
        progress,
    )
    .await?;
    let result = async {
        registry::compose(root, &layers, &ids, cx).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
    .await;
    history::recover(result, root, &checkpoint, cx, progress).await?;
    if !existing_mount(root, &layers, cx).await? {
        let prefix = root.join("prefix");
        ensure_empty_dir(&prefix).await?;
        cx.fvs()
            .await?
            .mount(&prefix, layers, Some(root.join("upper")))
            .await?;
    }
    Ok(())
}

async fn existing_mount(root: &Path, layers: &[Layer], context: &Context) -> Result<bool> {
    let prefix = root.join("prefix");
    let mounts = context.fvs().await?.list_mounts().await?;
    let Some(spec) = mounts
        .into_iter()
        .filter_map(|mount| mount.spec)
        .find(|spec| spec.mount_point == prefix.to_string_lossy())
    else {
        return Ok(false);
    };
    if spec.layers != layers
        || spec.upper_path.as_deref() != Some(root.join("upper").to_string_lossy().as_ref())
    {
        return Err(VirgoError::MountMismatch(prefix).into());
    }
    Ok(true)
}

pub(super) async fn stop(root: &Path, context: &Context) -> Result<()> {
    let prefix = root.join("prefix");
    let client = context.fvs().await?;
    if let Some(mount) = client.list_mounts().await?.into_iter().find(|mount| {
        mount
            .spec
            .as_ref()
            .is_some_and(|spec| spec.mount_point == prefix.to_string_lossy())
    }) {
        client
            .unmount(&mount, UnmountMode::Normal)
            .await
            .map_err(|source| crate::EnvironmentError::Cleanup {
                prefix,
                source: Box::new(source.into()),
            })?;
    }
    Ok(())
}

/// Refuses to mount over existing contents, which would otherwise be hidden.
async fn ensure_empty_dir(path: &Path) -> Result<()> {
    async_fs::create_dir_all(path).await?;
    if async_fs::read_dir(path).await?.try_next().await?.is_some() {
        return Err(VirgoError::DirtyMountpoint(path.to_path_buf()).into());
    }
    Ok(())
}
