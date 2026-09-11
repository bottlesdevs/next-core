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
    let mut components = Vec::new();
    let mut dependencies = Vec::new();
    for slot in Slot::iter().filter(|slot| !slot.is_runtime()) {
        let old = previous.component(slot);
        let new = candidate.component(slot);
        if old == new {
            continue;
        }
        if let Some(new) = new {
            let release = addons
                .component(new.id())
                .ok_or(AddonError::NotFound(new.id()))?;
            release.validate(&release.path(cx.directories())).await?;
            components.push(release);
        } else if let Some(old) = old {
            let release = addons
                .component(old.id())
                .ok_or(AddonError::NotFound(old.id()))?;
            removals.push(release);
        }
    }
    for new in &candidate.dependencies[previous.dependencies.len()..] {
        let downloaded = addons
            .dependency(new.id())
            .ok_or(AddonError::NotFound(new.id()))?;
        downloaded
            .validate(&downloaded.path(cx.directories()))
            .await?;
        dependencies.push(downloaded);
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
    let mut env_vars = previous.addon_env_vars();
    for release in removals {
        let result = uninstall(
            InstallInputs {
                prefix: &prefix,
                runner: runner.as_ref(),
                winebridge: &winebridge,
                env_vars: &mut env_vars,
                explicit_env_vars: &candidate.env_vars,
            },
            release.recipe(),
            release.id(),
            cancellation,
            |_| {
                progress.send_replace(Some(Progress::new(Stage::Removing)));
            },
        )
        .await;
        runtime::stop(runner.as_ref(), &prefix).await?;
        result?;
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
        let result = execute(
            InstallInputs {
                prefix: &prefix,
                runner: runner.as_ref(),
                winebridge: &winebridge,
                env_vars: &mut env_vars,
                explicit_env_vars: &candidate.env_vars,
            },
            &payload,
            resources,
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
