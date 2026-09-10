//! Addon operations on an owner's execution configuration and prefix data.

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{Environment, EnvironmentConfig, EnvironmentError, prefix};
use crate::{
    Addon, AddonError, Addons, Progress, Requirement, Slot, Stage,
    addons::{Artifact, InstallInputs, execute, replay_env_vars, uninstall},
    error::{Error, Result},
};

impl Environment {
    pub(crate) async fn set_component(
        &mut self,
        id: Uuid,
        addons: &Addons,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let component = addons.component(id).ok_or(AddonError::NotFound(id))?;
        if self
            .config
            .component(component.slot())
            .is_some_and(|installed| installed.id() == id)
        {
            return Ok(());
        }
        let mut candidate = self.config.clone();
        let needs_umu = component
            .requirements()
            .contains(&Requirement::Slot(Slot::Umu));
        if needs_umu && candidate.umu().is_none() {
            let umu = addons.latest_component(Slot::Umu).ok_or_else(|| {
                EnvironmentError::RequiresAddon {
                    required_by: Some(id),
                    requirements: vec![Requirement::Slot(Slot::Umu)],
                }
            })?;
            candidate
                .components
                .insert(Slot::Umu, Addon::from(umu.as_ref()));
        }
        candidate
            .components
            .insert(component.slot(), Addon::from(component.as_ref()));
        if component.slot() == Slot::Runner && !needs_umu {
            candidate.components.remove(&Slot::Umu);
        }
        candidate.validate_requirements()?;

        if component.slot().is_runtime() {
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            self.stop().await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            self.config = candidate;
            if component.slot() == Slot::Runner {
                progress.send_replace(Some(Progress::new(Stage::Rebuilding)));
                let installed = self
                    .config
                    .components
                    .values()
                    .filter(|component| !component.slot().is_runtime())
                    .map(Addon::id)
                    .chain(self.config.dependencies.iter().map(Addon::id))
                    .collect::<Vec<_>>();
                let runner = self
                    .config
                    .runner()
                    .load_runner(self.cx.directories(), self.config.umu())
                    .await?;
                let runner_key = self.config.runner().id().to_string();
                prefix::rebuild(
                    &mut self.config.storage,
                    runner.as_ref(),
                    &runner_key,
                    &installed,
                    &self.cx,
                )
                .await?;
            }
            return Ok(());
        }
        let replaced_id = self.config.component(component.slot()).map(Addon::id);
        let resources = vec![component.artifact(self.cx.directories())];
        self.install_item(
            candidate,
            id,
            replaced_id,
            resources,
            progress,
            cancellation,
        )
        .await
    }

    pub(crate) async fn remove_component(
        &mut self,
        slot: Slot,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let component = self
            .config
            .component(slot)
            .cloned()
            .ok_or(EnvironmentError::ComponentNotInstalled(slot))?;
        let mut candidate = self.config.clone();
        candidate.components.remove(&slot);
        candidate.validate_requirements()?;
        let item_id = component.id();
        let resources = vec![component.artifact(self.cx.directories())];
        let winebridge = self.config.winebridge().path(self.cx.directories());
        self.stop().await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.config = candidate;
        let runner = self
            .config
            .runner()
            .load_runner(self.cx.directories(), self.config.umu())
            .await?;
        let env_vars = &mut self.config.env_vars;
        prefix::uninstall(
            &mut self.config.storage,
            &self.root,
            item_id,
            async |prefix, restore_files| {
                uninstall(
                    InstallInputs {
                        prefix,
                        runner: runner.as_ref(),
                        winebridge: &winebridge,
                        env_vars,
                    },
                    &resources,
                    restore_files,
                    item_id,
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
        .await
    }

    pub(crate) async fn install(
        &mut self,
        id: Uuid,
        addons: &Addons,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let dependency = addons.dependency(id).ok_or(AddonError::NotFound(id))?;
        if self.config.dependency(id).is_some() {
            return Ok(());
        }
        let mut candidate = self.config.clone();
        candidate
            .dependencies
            .push(Addon::from(dependency.as_ref()));
        candidate.validate_requirements()?;
        let resources = dependency
            .artifacts()
            .iter()
            .map(|artifact| {
                Artifact::new(
                    dependency.path(self.cx.directories()).join(&artifact.path),
                    artifact.steps.clone(),
                )
            })
            .collect();
        self.install_item(candidate, id, None, resources, progress, cancellation)
            .await
    }

    /// The owner persists these changes only after the complete operation succeeds.
    async fn install_item(
        &mut self,
        candidate: EnvironmentConfig,
        item_id: Uuid,
        replaced_id: Option<Uuid>,
        resources: Vec<Artifact>,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        self.stop().await?;
        self.config = candidate;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let runner = self
            .config
            .runner()
            .load_runner(self.cx.directories(), self.config.umu())
            .await?;
        let winebridge = self.config.winebridge().path(self.cx.directories());
        let env_vars = &mut self.config.env_vars;
        prefix::install(
            &mut self.config.storage,
            &self.root,
            item_id,
            replaced_id,
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
        Ok(())
    }
}
