//! Load installed artifacts and prepare a stopped owner's Virgo composition.

mod artifacts;
pub(crate) use artifacts::VirgoManager;

use super::super::{EnvironmentState, history};
use crate::{
    Context, Progress, Stage,
    error::{Error, Result},
};
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
    let result = manager
        .layers
        .compose_and_mount(root, &base, &overlays, cancellation)
        .await;
    if result.is_err() {
        release(root, manager).await?;
    }
    history::recover(result, root, &checkpoint, cx, progress).await
}

pub(super) async fn release(root: &Path, manager: &VirgoManager) -> Result<()> {
    manager.layers.unmount_workspace(root).await?;
    for directory in ["prefix", "upper"] {
        crate::winebridge::WineBridgeClient::clear_discovery(&root.join(directory)).await?;
    }
    Ok(())
}
