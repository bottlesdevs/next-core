//! Owner coordination and registration lookup around environment operations.

use super::{Bottle, error::BottleError};
use crate::{
    LaunchSpec, Operation, Progress, Slot, Stage, environment,
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
    winebridge::WineBridgeClient,
};
use std::future::Future;
use uuid::Uuid;

impl Bottle {
    /// Lists Wine DLL overrides, reporting progress while starting the environment if needed.
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.with_bridge(async |environment| environment.list_dll_overrides().await)
    }

    /// Sets a Wine DLL loading mode, reporting progress during environment preparation.
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        if mode == DllOverrideMode::Unspecified {
            return Operation::new(|_, _| async {
                Err(crate::EnvironmentError::DllOverrideModeRequired.into())
            });
        }
        let dll = dll.into();
        self.with_bridge(async move |environment| environment.set_dll_override(dll, mode).await)
    }

    /// Removes a Wine DLL override, reporting preparation progress. Missing overrides succeed.
    pub fn unset_dll_override(&self, dll: impl Into<String>) -> Operation<()> {
        let dll = dll.into();
        self.with_bridge(async move |environment| environment.delete_dll_override(dll).await)
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
            let environment = environment::attach_or_start(
                &state.backend,
                &state.environment,
                &bottle.0.cx.directories().bottle(state.id),
                &bottle.0.cx,
                #[cfg(feature = "fvs")]
                &bottle.0.virgo,
                &progress,
                &cancellation,
            )
            .await?;
            environment::launch(&environment, id, program).await
        })
    }

    /// Runs an unregistered definition. The supplied UUID identifies the process group.
    pub fn launch(&self, id: Uuid, program: LaunchSpec) -> Operation<u32> {
        self.with_bridge(async move |environment| {
            environment::launch(&environment, id, &program).await
        })
    }

    /// Returns Windows processes without starting a stopped environment.
    pub async fn processes(&self) -> Result<Vec<Process>> {
        let _control = self.0.control.lock().await;
        let state = self.state()?;
        environment::processes(&self.0.cx.directories().bottle(state.id)).await
    }

    /// Terminates a registered program's UUID-keyed process group without starting Wine.
    pub async fn kill_program(&self, id: Uuid) -> Result<()> {
        let _control = self.0.control.lock().await;
        let state = self.state()?;
        if state.program(id).is_none() {
            return Err(BottleError::ProgramNotFound(id).into());
        }
        environment::kill(&self.0.cx.directories().bottle(state.id), id).await
    }

    /// Stops Wine before releasing storage, even when WineBridge cannot be reached.
    pub async fn stop(&self) -> Result<()> {
        let _control = self.0.control.lock().await;
        self.stop_locked().await
    }

    /// Select a downloaded component in a stopped environment.
    pub fn set_component(&self, id: Uuid) -> Operation<()> {
        self.update_software(move |environment, addons| environment.set_component(id, addons))
    }

    /// Remove a component unless another selected addon requires it.
    pub fn remove_component(&self, slot: Slot) -> Operation<()> {
        self.update_software(move |environment, _| {
            environment
                .components
                .remove(&slot)
                .ok_or(crate::EnvironmentError::ComponentNotInstalled(slot))?;
            Ok(())
        })
    }

    /// Install a downloaded dependency; an already-selected UUID is a no-op.
    pub fn install(&self, id: Uuid) -> Operation<()> {
        self.update_software(move |environment, addons| {
            if environment.dependency(id).is_none() {
                let dependency = addons
                    .dependency(id)
                    .ok_or(crate::AddonError::NotFound(id))?;
                environment
                    .dependencies
                    .push(crate::Addon::from(dependency.as_ref()));
            }
            Ok(())
        })
    }

    fn update_software(
        &self,
        update: impl FnOnce(&mut crate::EnvironmentState, &crate::Addons) -> Result<()> + Send + 'static,
    ) -> Operation<()> {
        let bottle = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let _control = cancellation
                .run_until_cancelled(bottle.0.control.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let previous = bottle.state()?;
            let mut draft = previous.as_ref().clone();
            update(&mut draft.environment, &bottle.0.addons)?;
            environment::apply(
                &previous.backend,
                &previous.environment,
                &draft.environment,
                &bottle.0.cx.directories().bottle(previous.id),
                &bottle.0.cx,
                &bottle.0.addons,
                &progress,
                &cancellation,
            )
            .await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            Self::save_state(&draft, &bottle.0.cx).await?;
            bottle.publish(draft);
            Ok(())
        })
    }

    // Caller must hold the owner lock, including through subsequent filesystem work.
    pub(super) async fn stop_locked(&self) -> Result<()> {
        let state = self.state()?;
        environment::stop(
            &state.backend,
            &state.environment,
            &self.0.cx.directories().bottle(state.id),
            &self.0.cx,
        )
        .await
    }

    /// Execute against a running environment while holding the owner lock.
    fn with_bridge<T, Fut>(
        &self,
        work: impl FnOnce(WineBridgeClient) -> Fut + Send + 'static,
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
            let environment = environment::attach_or_start(
                &state.backend,
                &state.environment,
                &bottle.0.cx.directories().bottle(state.id),
                &bottle.0.cx,
                #[cfg(feature = "fvs")]
                &bottle.0.virgo,
                &progress,
                &cancellation,
            )
            .await?;
            work(environment).await
        })
    }
}
