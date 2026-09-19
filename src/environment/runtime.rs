//! Wine process lifecycle shared by owner workflows and scratch artifact builds.
//! These helpers never mount, unmount, persist, or delete storage.

use crate::{error::Result, runner::Runner, winebridge::WineBridgeClient};
use std::path::Path;

/// Initialize Wine and stop its processes, including after initialization fails.
pub(super) async fn initialize(runner: &dyn Runner, prefix: &Path) -> Result<()> {
    let initialized = runner.wineboot(prefix, "--init").await;
    stop(runner, prefix).await?;
    initialized
}

/// Stops WineBridge and waits for wineserver, then removes discovery files.
/// Server control and discovery cleanup failures are returned; the caller owns storage cleanup.
pub(super) async fn stop(runner: &dyn Runner, prefix: &Path) -> Result<()> {
    if let Err(error) = WineBridgeClient::shutdown_existing(prefix).await {
        tracing::debug!(%error, "WineBridge shutdown failed; stopping wineserver");
    }
    for argument in ["-k", "-w"] {
        runner.wineserver(prefix, argument).await?;
    }
    WineBridgeClient::clear_discovery(prefix).await?;
    Ok(())
}
