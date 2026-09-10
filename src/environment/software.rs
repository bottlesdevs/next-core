//! Reconcile edited execution settings with persistent prefix data.

use std::path::Path;

use strum::IntoEnumIterator;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{EnvironmentConfig, EnvironmentError, prefix};
use crate::{
    Addon, AddonError, Addons, Context, Progress, Slot, Stage,
    addons::{Artifact, InstallInputs, execute, replay_env_vars, uninstall},
    error::{Error, Result},
};

/// Called while the owner is coordinated and stopped. The owner publishes
/// only after all prefix work succeeds. Existing dependencies remain installed.
pub(crate) async fn reconcile(
    previous: &EnvironmentConfig,
    candidate: &mut EnvironmentConfig,
    root: &Path,
    cx: &Context,
    addons: &Addons,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    if candidate.storage != previous.storage {
        return Err(EnvironmentError::InvalidEdit(
            "storage strategy and resolved layers are managed at creation and preparation",
        )
        .into());
    }
    if !candidate.dependencies.starts_with(&previous.dependencies) {
        return Err(EnvironmentError::InvalidEdit(
            "installed dependencies cannot be removed, replaced or reordered",
        )
        .into());
    }

    // Resolve every changed selection before any prefix work. Validation
    // above this layer sees the final batch, never intermediate selections.
    let mut removals = Vec::new();
    let mut installations = Vec::new();
    for slot in Slot::iter() {
        let old = previous.component(slot);
        let new = candidate.component(slot);
        if old == new {
            continue;
        }
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
            if !slot.is_runtime() {
                installations.push((
                    new.id(),
                    old.map(Addon::id),
                    vec![downloaded.artifact(cx.directories())],
                ));
            }
        } else if let Some(old) = old.filter(|_| !slot.is_runtime()) {
            removals.push((old.id(), vec![old.artifact(cx.directories())]));
        }
    }
    for new in &candidate.dependencies[previous.dependencies.len()..] {
        if previous.dependency(new.id()).is_some()
            || candidate
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
        let downloaded = addons
            .dependency(new.id())
            .ok_or(AddonError::NotFound(new.id()))?;
        if Addon::from(downloaded.as_ref()) != *new {
            return Err(EnvironmentError::InvalidEdit(
                "dependency selection must match its downloaded release",
            )
            .into());
        }
        let resources = downloaded
            .artifacts()
            .iter()
            .map(|artifact| {
                Artifact::new(
                    downloaded.path(cx.directories()).join(&artifact.path),
                    artifact.steps.clone(),
                )
            })
            .collect();
        installations.push((new.id(), None, resources));
    }

    let runner_changed = candidate.runner() != previous.runner();
    if !runner_changed && removals.is_empty() && installations.is_empty() {
        return Ok(());
    }
    let runner = candidate
        .runner()
        .load_runner(cx.directories(), candidate.umu())
        .await?;
    let winebridge = candidate.winebridge().path(cx.directories());
    let env_vars = &mut candidate.env_vars;

    for (id, resources) in &removals {
        prefix::uninstall(
            &mut candidate.storage,
            root,
            *id,
            async |prefix, restore_files| {
                uninstall(
                    InstallInputs {
                        prefix,
                        runner: runner.as_ref(),
                        winebridge: &winebridge,
                        env_vars,
                    },
                    resources,
                    restore_files,
                    *id,
                    cancellation,
                    |_| {
                        progress.send_replace(Some(Progress::new(Stage::Removing)));
                    },
                )
                .await
            },
            cx,
            cancellation,
            |event| {
                progress.send_replace(Some(event));
            },
        )
        .await?;
    }
    if runner_changed {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        progress.send_replace(Some(Progress::new(Stage::Rebuilding)));
        let installed = Slot::iter()
            .filter(|slot| !slot.is_runtime())
            .filter_map(|slot| previous.component(slot))
            .map(Addon::id)
            .filter(|id| !removals.iter().any(|(removed, _)| removed == id))
            .chain(previous.dependencies.iter().map(Addon::id))
            .collect::<Vec<_>>();
        prefix::rebuild(
            &mut candidate.storage,
            runner.as_ref(),
            &candidate.components[&Slot::Runner].id().to_string(),
            &installed,
            cx,
        )
        .await?;
    }
    for (id, replaced, resources) in installations {
        prefix::install(
            &mut candidate.storage,
            root,
            id,
            replaced,
            async |prefix| {
                execute(
                    InstallInputs {
                        prefix,
                        runner: runner.as_ref(),
                        winebridge: &winebridge,
                        env_vars,
                    },
                    &resources,
                    cancellation,
                    |_| {
                        progress.send_replace(Some(Progress::new(Stage::Configuring)));
                    },
                )
                .await
            },
            cx,
            cancellation,
            |event| {
                progress.send_replace(Some(event));
            },
        )
        .await?;
        replay_env_vars(env_vars, &resources);
    }
    Ok(())
}
