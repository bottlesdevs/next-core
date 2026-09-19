//! Initialize and mutate a conventional Wine prefix directly.

use super::super::runtime;
use crate::{
    Context, EnvironmentState, Progress, Slot, Stage,
    addons::{InstallInputs, execute, uninstall},
    error::Result,
};
use std::path::Path;
use strum::IntoEnumIterator;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Successful initialization leaves no Wine processes running; failed cleanup retains data.
pub(in crate::environment) async fn create(
    config: &EnvironmentState,
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

/// Apply frozen Standard selections directly to the stopped owner's prefix.
/// Removal uses the previous selection's recipe and prefix backups, without
/// consulting shared addon storage. Installation accesses payloads as steps run;
/// an input failure can leave earlier steps applied.
/// The caller requires a stopped owner. All work shares one maintenance session;
/// shutdown runs once after the batch, including failure or cancellation.
pub(in crate::environment) async fn apply(
    previous: &EnvironmentState,
    candidate: &EnvironmentState,
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
