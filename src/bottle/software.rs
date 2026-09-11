//! Owner coordination and registration lookup around environment operations.

use super::{Bottle, error::BottleError};
use crate::{
    Operation, ProgramSpec, Progress, Slot, Stage,
    environment::Environment,
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
};
use std::future::Future;
use uuid::Uuid;

impl Bottle {
    /// Lists Wine DLL overrides, reporting progress while starting the environment if needed.
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.with_environment(async |environment| environment.dll_overrides().await)
    }

    /// Sets a Wine DLL loading mode, reporting progress during environment preparation.
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        if mode == DllOverrideMode::Unspecified {
            return Operation::new(|_, _| async {
                Err(crate::EnvironmentError::DllOverrideModeRequired.into())
            });
        }
        let dll = dll.into();
        self.with_environment(async move |environment| {
            environment.set_dll_override(dll, mode).await
        })
    }

    /// Removes a Wine DLL override, reporting preparation progress. Missing overrides succeed.
    pub fn unset_dll_override(&self, dll: impl Into<String>) -> Operation<()> {
        let dll = dll.into();
        self.with_environment(async move |environment| environment.unset_dll_override(dll).await)
    }

    /// Resolves the latest registration under the owner lock before starting and launching.
    pub fn launch_program(&self, id: Uuid) -> Operation<u32> {
        let bottle = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let _control = cancellation
                .run_until_cancelled(bottle.0.control.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let state = bottle.state()?;
            let program = state.program(id).ok_or(BottleError::ProgramNotFound(id))?;
            let environment = Environment::attach_or_start(
                &state.environment,
                &bottle.0.cx.directories().bottle(state.id),
                &bottle.0.cx,
                #[cfg(feature = "fvs")]
                &bottle.0.virgo,
                &progress,
                &cancellation,
            )
            .await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            environment.launch(program).await
        })
    }

    /// Runs an unregistered definition. Its UUID identifies the process group.
    pub fn launch(&self, program: ProgramSpec) -> Operation<u32> {
        self.with_environment(async move |environment| environment.launch(&program).await)
    }

    /// Returns Windows processes without starting a stopped environment.
    pub async fn processes(&self) -> Result<Vec<Process>> {
        let _control = self.0.control.lock().await;
        let state = self.state()?;
        Environment::processes(&self.0.cx.directories().bottle(state.id)).await
    }

    /// Terminates a registered program's UUID-keyed process group without starting Wine.
    pub async fn kill_program(&self, id: Uuid) -> Result<()> {
        let _control = self.0.control.lock().await;
        let state = self.state()?;
        if state.program(id).is_none() {
            return Err(BottleError::ProgramNotFound(id).into());
        }
        Environment::kill(&self.0.cx.directories().bottle(state.id), id).await
    }

    /// Stops Wine before releasing storage, even when WineBridge cannot be reached.
    pub async fn stop(&self) -> Result<()> {
        let _control = self.0.control.lock().await;
        self.stop_locked().await
    }

    /// Selects a downloaded component in a stopped environment.
    /// A runner requiring UMU selects the latest downloaded UMU if necessary.
    pub fn set_component(&self, id: Uuid) -> Operation<()> {
        let addons = self.0.addons.clone();
        self.edit(move |state| state.environment.set_component(id, &addons))
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

    /// Installs a downloaded dependency in a stopped environment.
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

    // Caller must hold the owner lock, including through subsequent filesystem work.
    pub(super) async fn stop_locked(&self) -> Result<()> {
        let state = self.state()?;
        Environment::stop(
            &state.environment,
            &self.0.cx.directories().bottle(state.id),
            &self.0.cx,
        )
        .await
    }

    /// Execute against a running environment while holding the owner lock.
    fn with_environment<T, Fut>(
        &self,
        work: impl FnOnce(Environment) -> Fut + Send + 'static,
    ) -> Operation<T>
    where
        T: Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let bottle = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let _control = cancellation
                .run_until_cancelled(bottle.0.control.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let state = bottle.state()?;
            let environment = Environment::attach_or_start(
                &state.environment,
                &bottle.0.cx.directories().bottle(state.id),
                &bottle.0.cx,
                #[cfg(feature = "fvs")]
                &bottle.0.virgo,
                &progress,
                &cancellation,
            )
            .await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            work(environment).await
        })
    }
}
