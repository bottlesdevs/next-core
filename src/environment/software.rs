//! Reconcile edited execution settings with persistent prefix data.

use std::path::Path;

use strum::IntoEnumIterator;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "fvs")]
use super::Storage;
use super::{EnvironmentConfig, EnvironmentError, prefix};
use crate::{
    Addon, AddonError, Addons, Context, Progress, Slot, Stage,
    addons::{InstallInputs, execute, replay_env_vars, uninstall},
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
        let resources = downloaded.resources(cx.directories());
        installations.push((new.id(), None, resources));
    }

    let runner_changed = candidate.runner() != previous.runner();
    if !runner_changed && removals.is_empty() && installations.is_empty() {
        return Ok(());
    }
    super::Environment::stop(previous, root, cx).await?;
    let runner = candidate
        .runner()
        .load_runner(cx.directories(), candidate.umu())
        .await?;
    #[cfg(feature = "fvs")]
    if matches!(candidate.storage, Storage::Virgo { .. }) {
        for (id, _, _) in &installations {
            super::artifacts::prepare_addon(*id, candidate, addons, cx, progress, cancellation)
                .await?;
        }
    }
    if runner_changed {
        super::Environment::prepare(candidate, runner.as_ref(), addons, cx, cancellation).await?;
    }
    let winebridge = candidate.winebridge().path(cx.directories());

    for (id, resources) in &removals {
        transact(
            candidate,
            root,
            cx,
            cancellation,
            progress,
            async |config| {
                prefix::uninstall(
                    &mut config.storage,
                    root,
                    *id,
                    async |prefix, restore_files| {
                        uninstall(
                            InstallInputs {
                                prefix,
                                runner: runner.as_ref(),
                                winebridge: &winebridge,
                                env_vars: &mut config.env_vars,
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
                )
                .await
            },
        )
        .await?;
    }
    for (id, replaced, resources) in installations {
        transact(
            candidate,
            root,
            cx,
            cancellation,
            progress,
            async |config| {
                prefix::install(
                    &mut config.storage,
                    root,
                    id,
                    replaced,
                    async |prefix| {
                        execute(
                            InstallInputs {
                                prefix,
                                runner: runner.as_ref(),
                                winebridge: &winebridge,
                                env_vars: &mut config.env_vars,
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
                )
                .await
            },
        )
        .await?;
        replay_env_vars(&mut candidate.env_vars, &resources);
    }
    Ok(())
}

/// Coordinates one addon mutation. Only Virgo uses automatic checkpoints;
/// Standard keeps direct writes. Owner configuration is saved separately.
/// Failed shutdown/unmount returns before any rollback can touch live storage.
async fn transact(
    config: &mut EnvironmentConfig,
    root: &Path,
    cx: &Context,
    cancellation: &CancellationToken,
    progress: &watch::Sender<Option<Progress>>,
    work: impl for<'a> std::ops::AsyncFnOnce(&'a mut EnvironmentConfig) -> Result<()>,
) -> Result<()> {
    #[cfg(feature = "fvs")]
    let repository = fvs_rs::Repository {
        repository_path: root.display().to_string(),
        block_size: prefix::FVS_BLOCK_SIZE,
    };
    #[cfg(feature = "fvs")]
    let checkpoint = if matches!(config.storage, Storage::Virgo { .. }) {
        let stream = cx
            .fvs()
            .await?
            .commit_stream(&repository, prefix::AUTO_CHECKPOINT_MESSAGE.into())
            .await?;
        Some(
            prefix::finish_commit(stream, |event| {
                progress.send_replace(Some(Progress::transferring(
                    Stage::Checkpointing,
                    event.into(),
                )));
            })
            .await?,
        )
    } else {
        None
    };
    let _ = progress;
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let result = work(config).await;
    super::Environment::stop(config, root, cx).await?;
    let result = if result.is_ok() && cancellation.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        result
    };
    #[cfg(feature = "fvs")]
    if let (Err(error), Some(checkpoint)) = (&result, checkpoint) {
        let restored = async {
            let stream = cx
                .fvs()
                .await?
                .restore_stream(
                    &repository,
                    &checkpoint.state_id,
                    None::<&Path>,
                    true,
                    false,
                )
                .await?;
            prefix::finish_restore(stream, |event| {
                progress.send_replace(Some(Progress::transferring(Stage::Restoring, event.into())));
            })
            .await
        }
        .await;
        if let Err(failed) = restored {
            tracing::error!(%failed, "prefix rollback failed after {error}");
        }
    }
    result
}
