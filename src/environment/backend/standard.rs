//! Materializes software directly into a conventional Wine prefix.
//!
//! Creation initializes Wine and leaves no processes running. Later software
//! edits uninstall replaced components before installing their replacements and
//! newly appended dependencies in one maintenance session.

use super::super::runtime;
use crate::{
    Context, EnvironmentConfig, Progress, Slot, Stage,
    addons::{InstallInputs, execute, uninstall},
    error::Result,
};
use std::path::Path;
use strum::IntoEnumIterator;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Creates the prefix directory, initializes Wine, and stops its processes.
///
/// # Errors
///
/// Returns an error if directory creation, runner loading, Wine initialization,
/// or shutdown fails. A shutdown failure retains the prefix for diagnosis.
pub(in crate::environment) async fn create(
    config: &EnvironmentConfig,
    root: &Path,
    cx: &Context,
) -> Result<()> {
    let prefix = root.join("prefix");
    async_fs::create_dir_all(&prefix).await?;
    let runner = config
        .runner()
        .load_runner(cx.directories(), config.umu())
        .await?;
    runtime::initialize(runner.as_ref(), &prefix).await
}

/// Applies the difference between two frozen selections to a stopped prefix.
///
/// Removed components use the previous selection's embedded recipe and prefix
/// backups. Replacements and newly appended dependencies read their payloads as
/// each recipe runs, so a late failure can leave earlier steps applied. The
/// caller must stop the owner first; this function performs one final shutdown
/// after the batch whether application succeeds or fails.
///
/// # Errors
///
/// Returns an error if runner loading, uninstalling, installing, cancellation,
/// or the final Wine shutdown fails. A shutdown error takes precedence over the
/// application result.
pub(in crate::environment) async fn apply(
    previous: &EnvironmentConfig,
    candidate: &EnvironmentConfig,
    root: &Path,
    cx: &Context,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut removals = Vec::new();
    let mut components = Vec::new();
    let dependencies = &candidate.dependencies[previous.dependencies.len()..];
    for slot in Slot::iter().filter(|slot| !slot.is_runtime()) {
        let old = previous.component(slot);
        let new = candidate.component(slot);
        if old == new {
            continue;
        }
        if let Some(old) = old {
            removals.push(old);
        }
        if let Some(new) = new {
            components.push(new);
        }
    }
    if removals.is_empty() && components.is_empty() && dependencies.is_empty() {
        return Ok(());
    }
    let runner = candidate
        .runner()
        .load_runner(cx.directories(), candidate.umu())
        .await?;
    let prefix = root.join("prefix");
    let winebridge = candidate.winebridge().path(cx.directories());
    let staging = cx.directories().staging();
    let inputs = InstallInputs {
        prefix: &prefix,
        staging: &staging,
        runner: runner.as_ref(),
        winebridge: &winebridge,
    };
    let applied = async {
        for release in removals {
            uninstall(inputs, release.recipe(), release.id(), cancellation, |_| {
                progress.send_replace(Some(Progress::new(Stage::Removing)));
            })
            .await?;
        }
        let installations = components
            .iter()
            .map(|r| (r.path(cx.directories()), r.resources()))
            .chain(
                dependencies
                    .iter()
                    .map(|r| (r.path(cx.directories()), r.resources())),
            );
        for (payload, resources) in installations {
            execute(inputs, &payload, resources, true, cancellation, |_| {
                progress.send_replace(Some(Progress::new(Stage::Configuring)));
            })
            .await?;
        }
        Ok(())
    }
    .await;
    runtime::stop(runner.as_ref(), &prefix).await?;
    applied
}
