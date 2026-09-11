//! Reconcile edited execution settings with persistent prefix data.

use std::path::Path;

use strum::IntoEnumIterator;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "fvs")]
use super::Storage;
use super::{EnvironmentConfig, EnvironmentError};
use crate::{
    Addon, AddonError, Addons, Context, Progress, Slot, Stage,
    addons::{InstallInputs, execute, uninstall},
    error::Result,
};

/// Validate selections before lifecycle or checkpoint work; report whether reconciliation is needed.
pub(crate) fn validate_edit(
    previous: &EnvironmentConfig,
    candidate: &EnvironmentConfig,
    addons: &Addons,
) -> Result<bool> {
    if candidate.storage != previous.storage {
        return Err(EnvironmentError::InvalidEdit(
            "storage strategy and resolved layers are managed by the environment",
        )
        .into());
    }
    if matches!(candidate.storage, super::Storage::Standard)
        && !candidate.dependencies.starts_with(&previous.dependencies)
    {
        return Err(EnvironmentError::InvalidEdit(
            "installed dependencies cannot be removed, replaced or reordered",
        )
        .into());
    }

    let mut prefix_changed = candidate.dependencies != previous.dependencies;

    for slot in Slot::iter() {
        let old = previous.component(slot);
        let new = candidate.component(slot);
        if old == new {
            continue;
        }
        prefix_changed |= !slot.is_runtime() || slot == Slot::Runner;
        if let Some(new) = new {
            let downloaded = addons
                .component(new.id())
                .ok_or(AddonError::NotFound(new.id()))?;
            if Addon::from(downloaded.as_ref()) != *new {
                return Err(EnvironmentError::InvalidEdit(
                    "component selection must match its downloaded release",
                )
                .into());
            }
        }
    }
    for new in &candidate.dependencies {
        if candidate
            .dependencies
            .iter()
            .filter(|addon| addon.id() == new.id())
            .count()
            != 1
        {
            return Err(
                EnvironmentError::InvalidEdit("a dependency may only be selected once").into(),
            );
        }
        if previous.dependency(new.id()) == Some(new) {
            continue;
        }
        let downloaded = addons
            .dependency(new.id())
            .ok_or(AddonError::NotFound(new.id()))?;
        if Addon::from(downloaded.as_ref()) != *new {
            return Err(EnvironmentError::InvalidEdit(
                "dependency selection must match its downloaded release",
            )
            .into());
        }
    }

    Ok(prefix_changed)
}

/// Apply a validated edit that needs prefix work. The owner is stopped and
/// checkpoints Virgo before calling; configuration is saved before publication.
pub(crate) async fn reconcile(
    previous: &EnvironmentConfig,
    candidate: &mut EnvironmentConfig,
    root: &Path,
    cx: &Context,
    addons: &Addons,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    let runner = candidate
        .runner()
        .load_runner(cx.directories(), candidate.umu())
        .await?;
    #[cfg(feature = "fvs")]
    if matches!(candidate.storage, Storage::Virgo { .. }) {
        for id in candidate.ordered_addons() {
            super::artifacts::prepare_addon(id, candidate, addons, cx, progress, cancellation)
                .await?;
        }
        super::Environment::prepare(candidate, runner.as_ref(), addons, cx, cancellation).await?;
        let ids: Vec<_> = candidate.ordered_addons().collect();
        if let Storage::Virgo { layers } = &candidate.storage {
            super::registry::compose(root, layers, &ids, cx).await?;
        }
        return Ok(());
    }
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
        super::Environment::stop(candidate, root, cx).await?;
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
        super::Environment::stop(candidate, root, cx).await?;
        result?;
    }
    Ok(())
}
