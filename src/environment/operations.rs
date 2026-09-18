//! Consumer-driven mutations and runtime control under one environment lock.

#[cfg(feature = "fvs")]
use super::history;
use super::{Environment, EnvironmentOwnerState, prefix::standard, runtime};
use crate::{
    Edit, EnvironmentError, Operation, PrefixBackend, ProgramSpec, Progress, Stage,
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
        Operation::new(move |progress, cancellation| async move {
            let _control = environment.lock_control(&cancellation).await?;
            let previous = environment.state()?;
            let mut draft = previous.as_ref().clone();
            let result = callback(&mut Edit { draft: &mut draft })?;
            draft.validate()?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if draft == *previous {
                return Ok(result);
            }
            let before = previous.environment();
            let after = draft.environment();
            let software_changed =
                before.components != after.components || before.dependencies != after.dependencies;
            if software_changed
                && WineBridgeClient::try_connect(&environment.root.join("prefix"))
                    .await?
                    .is_some()
            {
                return Err(EnvironmentError::MustBeStopped.into());
            }
            // Once application succeeds, finish saving even if cancellation arrives.
            match (previous.backend(), software_changed) {
                (_, false) => environment.save(&draft).await?,
                (PrefixBackend::Standard, true) => {
                    standard::apply(
                        before,
                        after,
                        &environment.root,
                        &environment.context,
                        &progress,
                        &cancellation,
                    )
                    .await?;
                    environment.save(&draft).await?;
                }
                #[cfg(feature = "fvs")]
                (PrefixBackend::Virgo, true) => {
                    environment.release_storage(PrefixBackend::Virgo).await?;
                    let (base, overlays) = environment
                        .virgo
                        .prepare_artifacts(after, &progress, &cancellation)
                        .await?;
                    let checkpoint = history::capture(
                        &environment.root,
                        history::AUTO_CHECKPOINT_MESSAGE.into(),
                        false,
                        Stage::Checkpointing,
                        &environment.context,
                        &progress,
                    )
                    .await?;
                    let applied = async {
                        environment
                            .virgo
                            .layers
                            .prepare_workspace(&environment.root, &base, &overlays, &cancellation)
                            .await?;
                        environment.save(&draft).await
                    }
                    .await;
                    history::recover(
                        applied,
                        &environment.root,
                        &checkpoint,
                        &environment.context,
                        &progress,
                    )
                    .await?;
                }
            }
            environment.publish(draft);
            Ok(result)
        })
    }

    pub(crate) fn launch(
        self: &Arc<Self>,
        select: impl FnOnce(&T) -> Result<(Uuid, ProgramSpec)> + Send + 'static,
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
        if crate::utils::exists(&prefix).await? {
            let config = state.environment();
            let runner = config
                .runner()
                .load_runner(self.context.directories(), config.umu())
                .await?;
            runtime::stop(runner.as_ref(), &prefix).await?;
        }
        self.release_storage(state.backend()).await
    }

    /// Wine is stopped before releasing mounts and their discovery files.
    async fn release_storage(&self, backend: PrefixBackend) -> Result<()> {
        #[cfg(feature = "fvs")]
        if backend == PrefixBackend::Virgo {
            self.virgo.layers.unmount_workspace(&self.root).await?;
        }
        #[cfg(not(feature = "fvs"))]
        let _ = backend;
        WineBridgeClient::clear_discovery(&self.root.join("prefix")).await?;
        #[cfg(feature = "fvs")]
        if backend == PrefixBackend::Virgo {
            WineBridgeClient::clear_discovery(&self.root.join("upper")).await?;
        }
        Ok(())
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
        #[cfg(feature = "fvs")]
        if backend == PrefixBackend::Virgo {
            let (base, overlays) = self.virgo.composition(config).await?;
            let mounted = self
                .virgo
                .layers
                .mount_workspace(&self.root, &base, &overlays, cancellation)
                .await;
            if mounted.is_err() {
                self.release_storage(backend).await?;
            }
            mounted?;
        }
        if cancellation.is_cancelled() {
            self.release_storage(backend).await?;
            return Err(Error::Cancelled);
        }
        let command = config.wrappers.apply(WineBridgeClient::command(
            runner.as_ref(),
            &prefix,
            config.winebridge().path(self.context.directories()),
            vars.iter(),
        ));
        let result = WineBridgeClient::connect_or_spawn(&prefix, command)
            .await
            .and_then(|bridge| {
                if cancellation.is_cancelled() {
                    Err(Error::Cancelled)
                } else {
                    Ok(bridge)
                }
            });
        if result.is_err() {
            runtime::stop(runner.as_ref(), &prefix).await?;
            self.release_storage(backend).await?;
        }
        result
    }
}
