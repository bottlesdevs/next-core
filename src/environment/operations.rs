//! Consumer-driven mutations and runtime control under one environment lock.

use super::{Environment, EnvironmentOwnerState, runtime};
use crate::{
    Addon, AddonError, Addons, Edit, EnvironmentError, EnvironmentState, LaunchSpec, Operation,
    Progress, Slot, Stage,
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
    winebridge::WineBridgeClient,
};
use std::{future::Future, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

impl<T: EnvironmentOwnerState> Environment<T> {
    pub(crate) fn edit<R: Send + 'static>(
        self: &Arc<Self>,
        callback: impl FnOnce(&mut Edit<'_, T>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        let environment = self.clone();
        Operation::new(move |_, cancellation| async move {
            let _control = environment.lock_control(&cancellation).await?;
            let previous = environment.state()?;
            let mut draft = previous.as_ref().clone();
            let result = callback(&mut Edit { draft: &mut draft })?;
            draft.validate()?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            environment.save(&draft).await?;
            environment.publish(draft);
            Ok(result)
        })
    }

    pub(crate) fn set_component(self: &Arc<Self>, id: Uuid) -> Operation<()> {
        self.update_software(move |state, addons| state.set_component(id, addons))
    }

    pub(crate) fn remove_component(self: &Arc<Self>, slot: Slot) -> Operation<()> {
        self.update_software(move |state, _| {
            state
                .components
                .remove(&slot)
                .ok_or(EnvironmentError::ComponentNotInstalled(slot))?;
            Ok(())
        })
    }

    pub(crate) fn install(self: &Arc<Self>, id: Uuid) -> Operation<()> {
        self.update_software(move |state, addons| {
            if state.dependency(id).is_none() {
                let dependency = addons.dependency(id).ok_or(AddonError::NotFound(id))?;
                state.dependencies.push(Addon::from(dependency.as_ref()));
            }
            Ok(())
        })
    }

    fn update_software(
        self: &Arc<Self>,
        update: impl FnOnce(&mut EnvironmentState, &Addons) -> Result<()> + Send + 'static,
    ) -> Operation<()> {
        let environment = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let _control = environment.lock_control(&cancellation).await?;
            let previous = environment.state()?;
            let mut draft = previous.as_ref().clone();
            update(draft.environment_mut(), &environment.addons)?;
            let before = previous.environment();
            let after = draft.environment();
            let backend = previous.backend();
            after.validate_edit(before, &environment.addons)?;
            backend.validate_edit(before, after)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if before == after {
                return Ok(());
            }
            if WineBridgeClient::try_connect(&environment.root.join("prefix"))
                .await?
                .is_some()
            {
                return Err(EnvironmentError::MustBeStopped.into());
            }
            environment.stop_locked().await?;
            backend
                .apply(
                    before,
                    after,
                    &environment.root,
                    &environment.context,
                    &environment.addons,
                    &progress,
                    &cancellation,
                )
                .await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            environment.save(&draft).await?;
            environment.publish(draft);
            Ok(())
        })
    }

    pub(crate) fn launch(
        self: &Arc<Self>,
        select: impl FnOnce(&T) -> Result<(Uuid, LaunchSpec)> + Send + 'static,
    ) -> Operation<u32> {
        let environment = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let _control = environment.lock_control(&cancellation).await?;
            let state = environment.state()?;
            let (id, launch) = select(&state)?;
            launch.validate()?;
            let bridge = environment
                .attach_or_start(&state, &progress, &cancellation)
                .await?;
            bridge
                .launch_process(
                    id,
                    launch.executable().into(),
                    launch.args().to_vec(),
                    launch.working_directory().map(str::to_owned),
                    launch.new_console(),
                )
                .await
        })
    }

    pub(crate) async fn processes(&self) -> Result<Vec<Process>> {
        let _control = self.control.lock().await;
        self.state()?;
        match WineBridgeClient::try_connect(&self.root.join("prefix")).await? {
            Some(bridge) => bridge.list_processes().await,
            None => Ok(Vec::new()),
        }
    }

    pub(crate) async fn kill(&self, select: impl FnOnce(&T) -> Result<Uuid>) -> Result<()> {
        let _control = self.control.lock().await;
        let state = self.state()?;
        let id = select(&state)?;
        if let Some(bridge) = WineBridgeClient::try_connect(&self.root.join("prefix")).await? {
            bridge.kill_process(id).await?;
        }
        Ok(())
    }

    pub(crate) async fn stop(&self) -> Result<()> {
        let _control = self.control.lock().await;
        self.stop_locked().await
    }

    /// Caller retains coordination through subsequent filesystem work.
    pub(crate) async fn stop_locked(&self) -> Result<()> {
        let state = self.state()?;
        let prefix = self.root.join("prefix");
        if !crate::utils::exists(&prefix).await? {
            return Ok(());
        }
        let config = state.environment();
        let runner = config
            .runner()
            .load_runner(self.context.directories(), config.umu())
            .await?;
        runtime::stop(runner.as_ref(), &prefix).await?;
        state.backend().release(&self.root, &self.context).await
    }

    pub(crate) fn dll_overrides(self: &Arc<Self>) -> Operation<Vec<DllOverride>> {
        self.with_bridge(async |bridge| bridge.list_dll_overrides().await)
    }

    pub(crate) fn set_dll_override(
        self: &Arc<Self>,
        dll: String,
        mode: DllOverrideMode,
    ) -> Operation<()> {
        if mode == DllOverrideMode::Unspecified {
            return Operation::new(|_, _| async {
                Err(EnvironmentError::DllOverrideModeRequired.into())
            });
        }
        self.with_bridge(async move |bridge| bridge.set_dll_override(dll, mode).await)
    }

    pub(crate) fn unset_dll_override(self: &Arc<Self>, dll: String) -> Operation<()> {
        self.with_bridge(async move |bridge| bridge.delete_dll_override(dll).await)
    }

    fn with_bridge<R, Fut>(
        self: &Arc<Self>,
        work: impl FnOnce(WineBridgeClient) -> Fut + Send + 'static,
    ) -> Operation<R>
    where
        R: Send + 'static,
        Fut: Future<Output = Result<R>> + Send + 'static,
    {
        let environment = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let _control = environment.lock_control(&cancellation).await?;
            let state = environment.state()?;
            let bridge = environment
                .attach_or_start(&state, &progress, &cancellation)
                .await?;
            work(bridge).await
        })
    }

    async fn attach_or_start(
        &self,
        state: &T,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<WineBridgeClient> {
        progress.send_replace(Some(Progress::new(Stage::Preparing)));
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let prefix = self.root.join("prefix");
        if let Some(bridge) = WineBridgeClient::try_connect(&prefix).await? {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            return Ok(bridge);
        }
        let config = state.environment();
        let backend = state.backend();
        let vars = config.effective_env_vars();
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.stop_locked().await?;
        let runner = config
            .runner()
            .load_runner(self.context.directories(), config.umu())
            .await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let result = async {
            backend
                .prepare(
                    config,
                    runner.as_ref(),
                    &self.root,
                    &self.context,
                    #[cfg(feature = "fvs")]
                    &self.virgo,
                    progress,
                    cancellation,
                )
                .await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let command = config.wrappers.apply(WineBridgeClient::command(
                runner.as_ref(),
                &prefix,
                config.winebridge().path(self.context.directories()),
                vars.iter(),
            ));
            WineBridgeClient::connect_or_spawn(&prefix, command).await
        }
        .await;
        runtime::finish_start(result, cancellation, async {
            runtime::stop(runner.as_ref(), &prefix).await?;
            backend.release(&self.root, &self.context).await
        })
        .await
    }
}
