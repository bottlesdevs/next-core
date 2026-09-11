//! Resolve shared artifacts and prepare a stopped owner's Virgo composition.

mod artifacts;
mod registry;

use super::super::{EnvironmentConfig, history};
use crate::{
    Context, Progress, Stage,
    error::{Error, Result},
    runner::Runner,
};
use futures_lite::StreamExt;
use fvs_rs::{Layer, UnmountMode};
use std::path::Path;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Builds the selected Virgo composition while the owner is coordinated and stopped.
pub(super) async fn prepare(
    config: &EnvironmentConfig,
    runner: &dyn Runner,
    root: &Path,
    cx: &Context,
    addons: &crate::Addons,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    let base = artifacts::prepare_base(addons, cx, cancellation).await?;
    let adapter =
        artifacts::prepare_adapter(config.runner().id(), runner, &base, cx, cancellation).await?;
    let ids = config
        .ordered_components()
        .map(crate::Addon::id)
        .chain(config.dependencies.iter().map(crate::Addon::id));
    let mut built = Vec::new();
    for id in ids {
        built.push(artifacts::prepare_addon(id, &base, addons, cx, progress, cancellation).await?);
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
    let patches = std::iter::once(&adapter)
        .chain(built.iter())
        .map(|artifact| artifact.registry.clone())
        .collect();
    let result = async {
        registry::compose(root, &base.registry, patches).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
    .await;
    history::recover(result, root, &checkpoint, cx, progress).await?;
    let mut layers = vec![base.layer, adapter.layer];
    layers.extend(built.into_iter().map(|addon| addon.layer));
    mount(root, layers, cx).await
}

/// Mount resolved layers after the environment workflow has stopped Wine.
async fn mount(root: &Path, layers: Vec<Layer>, cx: &Context) -> Result<()> {
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

pub(super) async fn release(root: &Path, context: &Context) -> Result<()> {
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

/// Virgo-specific failures carried by [`crate::error::Error::Virgo`].
#[derive(Debug, thiserror::Error)]
pub enum VirgoError {
    #[error("no Soda runner release in the current component catalog")]
    SodaNotInCatalog,
    #[error("invalid Soda semantic version: {0}")]
    InvalidSodaVersion(String),
    #[error("download Soda {version} ({id}) before building the Virgo base or an addon layer")]
    SodaNotDownloaded { id: uuid::Uuid, version: String },

    /// Virgo cannot mount a prefix over a nonempty mountpoint.
    #[error("mountpoint is not empty: {0}")]
    DirtyMountpoint(std::path::PathBuf),
    #[error(
        "mounted layers or writable upper differ from the selected composition at {0}; call stop() and retry"
    )]
    MountMismatch(std::path::PathBuf),
    /// A published artifact has an unsupported format or incomplete installed effects.
    #[error("invalid Virgo artifact: {0}")]
    InvalidArtifact(std::path::PathBuf),
    /// Registry data could not be converted while building a Virgo layer.
    #[error("failed to process Virgo registry data: {0}")]
    Registry(String),
}
