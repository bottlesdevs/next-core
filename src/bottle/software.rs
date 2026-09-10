//! Public bottle operations coordinated around a private cached environment.

use std::ops::AsyncFnOnce;

use uuid::Uuid;

use super::{Bottle, BottleState, error::BottleError};
use crate::{
    Context, Operation, ProgramSpec, Progress, Slot, Stage,
    environment::Environment,
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
};

impl Bottle {
    /// Lists Wine DLL overrides, starting the environment if necessary.
    pub async fn dll_overrides(&self) -> Result<Vec<DllOverride>> {
        self.with_environment(async |environment| environment.dll_overrides().await)
            .await
    }

    /// Sets a Wine DLL loading mode, starting the environment if necessary.
    pub async fn set_dll_override(
        &self,
        dll: impl Into<String>,
        mode: DllOverrideMode,
    ) -> Result<()> {
        if mode == DllOverrideMode::Unspecified {
            return Err(crate::EnvironmentError::DllOverrideModeRequired.into());
        }
        let dll = dll.into();
        self.with_environment(async move |environment| {
            environment.set_dll_override(dll, mode).await
        })
        .await
    }

    /// Removes a Wine DLL override. Removing a missing override succeeds.
    pub async fn unset_dll_override(&self, dll: impl Into<String>) -> Result<()> {
        let dll = dll.into();
        self.with_environment(async move |environment| environment.unset_dll_override(dll).await)
            .await
    }

    /// Launches the latest registration with this bottle's execution settings.
    ///
    /// The lazy operation resolves the definition under the owner lock and returns
    /// the initial Windows process ID. The process continues after it completes.
    pub fn launch_program(&self, id: Uuid) -> Operation<u32> {
        self.launch_with(move |state| {
            state
                .program(id)
                .cloned()
                .ok_or_else(|| BottleError::ProgramNotFound(id).into())
        })
    }

    /// Runs an unregistered launch definition with this bottle's settings.
    /// This does not add a library entry. The UUID still identifies its process group.
    pub fn launch(&self, program: ProgramSpec) -> Operation<u32> {
        self.launch_with(move |_| Ok(program))
    }

    fn launch_with(
        &self,
        resolve: impl FnOnce(&BottleState) -> Result<ProgramSpec> + Send + 'static,
    ) -> Operation<u32> {
        let bottle = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let mut cached = cancellation
                .run_until_cancelled(bottle.0.environment.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let state = bottle.state()?;
            let program = resolve(&state)?;
            let environment = Self::environment(&mut cached, &state, &bottle.0.cx).await?;
            environment.launch_program(&program, &cancellation).await
        })
    }

    /// Returns Windows processes, starting the environment if necessary.
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.with_environment(async |environment| environment.processes().await)
            .await
    }

    /// Terminates a registered program's UUID-keyed process group.
    /// This starts the environment if necessary and leaves it available afterward.
    pub async fn kill_program(&self, id: Uuid) -> Result<()> {
        let mut cached = self.0.environment.lock().await;
        let state = self.state()?;
        if state.program(id).is_none() {
            return Err(BottleError::ProgramNotFound(id).into());
        }
        Self::environment(&mut cached, &state, &self.0.cx)
            .await?
            .kill(id)
            .await
    }

    /// Stops WineBridge, wineserver and storage, then clears the cached environment.
    /// Storage is released only after shutdown succeeds.
    pub async fn stop(&self) -> Result<()> {
        let mut environment = self.0.environment.lock().await;
        let state = self.state()?;
        Self::stop_state(&state, &self.0.cx, &mut environment).await
    }

    /// Selects a downloaded component in a stopped environment.
    /// A runner requiring UMU selects the latest downloaded UMU if necessary.
    pub fn set_component(&self, id: Uuid) -> Operation<()> {
        let addons = self.0.addons.clone();
        self.edit(move |state| {
            let component = addons
                .component(id)
                .ok_or(crate::AddonError::NotFound(id))?;
            let config = &mut state.environment;
            if config
                .component(component.slot())
                .is_some_and(|old| old.id() == id)
            {
                return Ok(());
            }
            let needs_umu = component
                .requirements()
                .contains(&crate::Requirement::Slot(Slot::Umu));
            if needs_umu && config.umu().is_none() {
                let umu = addons.latest_component(Slot::Umu).ok_or_else(|| {
                    crate::EnvironmentError::RequiresAddon {
                        required_by: Some(id),
                        requirements: vec![crate::Requirement::Slot(Slot::Umu)],
                    }
                })?;
                config
                    .components
                    .insert(Slot::Umu, crate::Addon::from(umu.as_ref()));
            }
            config
                .components
                .insert(component.slot(), crate::Addon::from(component.as_ref()));
            if component.slot() == Slot::Runner && !needs_umu {
                config.components.remove(&Slot::Umu);
            }
            Ok(())
        })
    }

    /// Removes a component from a stopped environment unless another addon requires it.
    pub fn remove_component(&self, slot: Slot) -> Operation<()> {
        self.edit(move |state| {
            state
                .environment
                .components
                .remove(&slot)
                .ok_or(crate::EnvironmentError::ComponentNotInstalled(slot))?;
            Ok(())
        })
    }

    /// Permanently installs a downloaded dependency in a stopped environment.
    /// Reinstalling its UUID is a no-op.
    pub fn install(&self, id: Uuid) -> Operation<()> {
        let addons = self.0.addons.clone();
        self.edit(move |state| {
            if state.environment.dependency(id).is_none() {
                let dependency = addons
                    .dependency(id)
                    .ok_or(crate::AddonError::NotFound(id))?;
                state
                    .environment
                    .dependencies
                    .push(crate::Addon::from(dependency.as_ref()));
            }
            Ok(())
        })
    }

    pub(super) async fn stop_state(
        state: &BottleState,
        cx: &Context,
        cached: &mut Option<Environment>,
    ) -> Result<()> {
        Environment::stop(&state.environment, &cx.directories().bottle(state.id), cx).await?;
        *cached = None;
        Ok(())
    }

    async fn environment<'a>(
        cached: &'a mut Option<Environment>,
        state: &BottleState,
        cx: &Context,
    ) -> Result<&'a Environment> {
        if cached.is_none() {
            *cached = Some(
                Environment::attach_or_start(
                    &state.environment,
                    cx.directories().bottle(state.id),
                    cx.clone(),
                )
                .await?,
            );
        }
        Ok(cached.as_ref().expect("environment initialized"))
    }

    async fn with_environment<F, T>(&self, work: F) -> Result<T>
    where
        F: for<'a> AsyncFnOnce(&'a Environment) -> Result<T>,
    {
        let mut cached = self.0.environment.lock().await;
        let state = self.state()?;
        let environment = Self::environment(&mut cached, &state, &self.0.cx).await?;
        work(environment).await
    }
}
