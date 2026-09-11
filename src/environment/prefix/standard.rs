//! Initialize and mutate a conventional Wine prefix directly.

use super::super::runtime;
use crate::{
    AddonError, Addons, Context, EnvironmentConfig, EnvironmentError, Progress, Slot, Stage,
    addons::{InstallInputs, execute, uninstall},
    error::Result,
};
use std::path::Path;
use strum::IntoEnumIterator;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub(super) async fn create(config: &EnvironmentConfig, root: &Path, cx: &Context) -> Result<()> {
    let prefix = root.join("prefix");
    async_fs::create_dir_all(&prefix).await?;
    let runner = config
        .runner()
        .load_runner(cx.directories(), config.umu())
        .await?;
    runtime::initialize(runner.as_ref(), &prefix).await
}

pub(super) fn validate_edit(
    previous: &EnvironmentConfig,
    candidate: &EnvironmentConfig,
) -> Result<()> {
    if !candidate.dependencies.starts_with(&previous.dependencies) {
        return Err(EnvironmentError::InvalidEdit(
            "installed dependencies cannot be removed, replaced or reordered",
        )
        .into());
    }
    Ok(())
}

/// Apply validated Standard selections directly to the stopped owner's prefix.
pub(super) async fn apply(
    previous: &EnvironmentConfig,
    candidate: &EnvironmentConfig,
    root: &Path,
    cx: &Context,
    addons: &Addons,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut removals = Vec::new();
    let mut installations = Vec::new();
    for slot in Slot::iter().filter(|slot| !slot.is_runtime()) {
        let old = previous.component(slot);
        let new = candidate.component(slot);
        if old == new {
            continue;
        }
        if let Some(new) = new {
            installations.push(vec![new.artifact(cx.directories())]);
        } else if let Some(old) = old {
            removals.push((old.id(), vec![old.artifact(cx.directories())]));
        }
    }
    for new in &candidate.dependencies[previous.dependencies.len()..] {
        let downloaded = addons
            .dependency(new.id())
            .ok_or(AddonError::NotFound(new.id()))?;
        installations.push(downloaded.resources(cx.directories()));
    }
    if removals.is_empty() && installations.is_empty() {
        return Ok(());
    }
    let runner = candidate
        .runner()
        .load_runner(cx.directories(), candidate.umu())
        .await?;
    let prefix = root.join("prefix");
    let winebridge = candidate.winebridge().path(cx.directories());
    let mut env_vars = previous.addon_env_vars(addons)?;
    for (id, resources) in removals {
        let result = uninstall(
            InstallInputs {
                prefix: &prefix,
                runner: runner.as_ref(),
                winebridge: &winebridge,
                env_vars: &mut env_vars,
                explicit_env_vars: &candidate.env_vars,
            },
            &resources,
            id,
            cancellation,
            |_| {
                progress.send_replace(Some(Progress::new(Stage::Removing)));
            },
        )
        .await;
        runtime::stop(runner.as_ref(), &prefix).await?;
        result?;
    }
    for resources in installations {
        let result = execute(
            InstallInputs {
                prefix: &prefix,
                runner: runner.as_ref(),
                winebridge: &winebridge,
                env_vars: &mut env_vars,
                explicit_env_vars: &candidate.env_vars,
            },
            &resources,
            cancellation,
            |_| {
                progress.send_replace(Some(Progress::new(Stage::Configuring)));
            },
        )
        .await;
        runtime::stop(runner.as_ref(), &prefix).await?;
        result?;
    }
    Ok(())
}
