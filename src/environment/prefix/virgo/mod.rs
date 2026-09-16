//! Load installed artifacts and prepare a stopped owner's Virgo composition.

mod artifacts;
use crate::virgo::{VirgoError, registry};
pub(crate) use artifacts::VirgoManager;

use super::super::{EnvironmentState, history};
use crate::{
    Context, Progress, Stage,
    error::{Error, Result},
};
use futures_lite::StreamExt;
use fvs_rs::{Layer, UnmountMode};
use std::path::Path;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Assemble installed layers while the owner is coordinated and stopped.
pub(super) async fn prepare(
    config: &EnvironmentState,
    root: &Path,
    cx: &Context,
    manager: &VirgoManager,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    let artifacts::VirgoComposition { base, overlays } =
        artifacts::load(config, &manager.layers).await?;
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
    let patches = overlays
        .iter()
        .map(|artifact| artifact.registry.clone())
        .collect();
    let result = async {
        registry::compose(root, &base.registry, patches).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut layers = vec![base.layer];
        layers.extend(overlays.into_iter().map(|artifact| artifact.layer));
        mount(root, layers, cx).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        release(root, cx).await?;
    }
    history::recover(result, root, &checkpoint, cx, progress).await
}

/// Mount resolved layers after the environment workflow has stopped Wine.
async fn mount(root: &Path, layers: Vec<Layer>, cx: &Context) -> Result<()> {
    let prefix = root.join("prefix");
    ensure_empty_dir(&prefix).await?;
    cx.fvs()
        .mount(&prefix, layers, Some(root.join("upper")))
        .await?;
    Ok(())
}

pub(super) async fn release(root: &Path, context: &Context) -> Result<()> {
    let prefix = root.join("prefix");
    if crate::utils::exists(&prefix).await? {
        let client = context.fvs();
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
    }
    for directory in ["prefix", "upper"] {
        crate::winebridge::WineBridgeClient::clear_discovery(&root.join(directory)).await?;
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
