//! Wine process lifecycle shared by owner workflows and scratch artifact builds.
//! These helpers never mount, unmount, persist, or delete storage.

use crate::{EnvironmentError, error::Result, runner::Runner, winebridge::WineBridgeClient};
use std::path::Path;

/// Initialize Wine and stop its processes, including after initialization fails.
pub(super) async fn initialize(runner: &dyn Runner, prefix: &Path) -> Result<()> {
    let initialized = runner.wineboot(prefix, "--init").await;
    stop(runner, prefix).await?;
    initialized
}

/// Stops WineBridge and waits for wineserver; the caller owns storage cleanup.
pub(super) async fn stop(runner: &dyn Runner, prefix: &Path) -> Result<()> {
    if let Err(error) = WineBridgeClient::shutdown_existing(prefix).await {
        tracing::debug!(%error, "WineBridge shutdown failed; stopping wineserver");
    }
    for argument in ["-k", "-w"] {
        runner
            .wineserver(prefix, argument)
            .await
            .map_err(|source| EnvironmentError::Cleanup {
                prefix: prefix.to_path_buf(),
                source: Box::new(source),
            })?;
    }
    if let Err(error) = WineBridgeClient::clear_discovery(prefix).await {
        tracing::warn!(%error, "could not remove WineBridge discovery after shutdown");
    }
    Ok(())
}

/// Finish startup under the caller's coordination lock. Failed or cancelled startup
/// must stop its processes and release storage before the caller can return.
pub(super) async fn finish_start<T>(
    result: Result<T>,
    cancellation: &tokio_util::sync::CancellationToken,
    cleanup: impl std::future::Future<Output = Result<()>>,
) -> Result<T> {
    let result = result.and_then(|value| {
        if cancellation.is_cancelled() {
            Err(crate::error::Error::Cancelled)
        } else {
            Ok(value)
        }
    });
    if result.is_err() {
        cleanup.await?;
    }
    result
}
