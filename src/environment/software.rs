//! Reconcile edited execution settings with persistent prefix data.

use strum::IntoEnumIterator;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{Environment, EnvironmentConfig, EnvironmentError, prefix};
use crate::{
    Addon, AddonError, Addons, Progress, Slot, Stage,
    addons::{Artifact, InstallInputs, execute, replay_env_vars, uninstall},
    error::{Error, Result},
};

impl Environment {
    /// Called while the owner is coordinated and stopped. The owner publishes
    /// only after all prefix work succeeds. Existing dependencies remain installed.
    pub(crate) async fn reconcile(
        &mut self,
        candidate: EnvironmentConfig,
        addons: &Addons,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        if candidate.storage != self.config.storage {
            return Err(EnvironmentError::InvalidEdit(
                "storage strategy and resolved layers are managed at creation and preparation",
            )
            .into());
        }
        if !candidate
            .dependencies
            .starts_with(&self.config.dependencies)
        {
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
            let old = self.config.component(slot);
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
                        vec![downloaded.artifact(self.cx.directories())],
                    ));
                }
            } else if let Some(old) = old.filter(|_| !slot.is_runtime()) {
                removals.push((old.id(), vec![old.artifact(self.cx.directories())]));
            }
        }
        for new in &candidate.dependencies[self.config.dependencies.len()..] {
            if self.config.dependency(new.id()).is_some()
                || candidate
                    .dependencies
                    .iter()
                    .filter(|addon| addon.id() == new.id())
                    .count()
                    != 1
            {
                return Err(EnvironmentError::InvalidEdit(
                    "a dependency may only be selected once",
                )
                .into());
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
                        downloaded.path(self.cx.directories()).join(&artifact.path),
                        artifact.steps.clone(),
                    )
                })
                .collect();
            installations.push((new.id(), None, resources));
        }

        let runner_changed = candidate.runner() != self.config.runner();
        if !runner_changed && removals.is_empty() && installations.is_empty() {
            self.config = candidate;
            return Ok(());
        }
        let runner = candidate
            .runner()
            .load_runner(self.cx.directories(), candidate.umu())
            .await?;
        let winebridge = candidate.winebridge().path(self.cx.directories());
        let previous = std::mem::replace(&mut self.config, candidate);
        let env_vars = &mut self.config.env_vars;

        for (id, resources) in &removals {
            prefix::uninstall(
                &mut self.config.storage,
                &self.root,
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
                &self.cx,
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
                &mut self.config.storage,
                runner.as_ref(),
                &self.config.components[&Slot::Runner].id().to_string(),
                &installed,
                &self.cx,
            )
            .await?;
        }
        for (id, replaced, resources) in installations {
            prefix::install(
                &mut self.config.storage,
                &self.root,
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
                &self.cx,
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
}
