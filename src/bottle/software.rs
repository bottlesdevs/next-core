use std::future::Future;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    Context, Operation, Progress, Stage,
    addons::{Addon, RunnerComponent, installer::InstallResource},
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
    runner::shutdown_prefix,
    winebridge::WineBridgeClient,
};

use super::{
    error::BottleError,
    state::{Bottle, BottleState},
};

impl Bottle {
    pub async fn dll_overrides(&self) -> Result<Vec<DllOverride>> {
        self.with_bridge(|bridge| async move {
            match bridge.list_dll_overrides().await {
                Ok(overrides) => Ok(overrides),
                Err(Error::Status(status)) if status.code() == tonic::Code::NotFound => {
                    Ok(Vec::new())
                }
                Err(error) => Err(error),
            }
        })
        .await
    }

    pub async fn set_dll_override(
        &self,
        dll: impl Into<String>,
        mode: DllOverrideMode,
    ) -> Result<()> {
        let dll = dll.into();
        if mode == DllOverrideMode::Unspecified {
            return Err(BottleError::DllOverrideModeRequired.into());
        }
        self.with_bridge(move |bridge| async move { bridge.set_dll_override(dll, mode).await })
            .await
    }

    pub async fn unset_dll_override(&self, dll: impl Into<String>) -> Result<()> {
        let dll = dll.into();
        self.with_bridge(move |bridge| async move {
            match bridge.delete_dll_override(dll).await {
                Err(Error::Status(status)) if status.code() == tonic::Code::NotFound => Ok(()),
                result => result,
            }
        })
        .await
    }

    /// Launch a tracked program, starting WineBridge if it is not already running.
    pub async fn run(&self, id: Uuid) -> Result<u32> {
        let program = self
            .state()?
            .program(id)
            .cloned()
            .ok_or(BottleError::ProgramNotFound(id))?;
        self.with_bridge(move |bridge| async move {
            bridge
                .launch_process(
                    program.id,
                    program.executable,
                    program.args,
                    program.working_directory,
                    program.new_console,
                )
                .await
        })
        .await
    }

    /// List processes, starting WineBridge if it is not already running.
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.with_bridge(|bridge| async move { bridge.list_processes().await })
            .await
    }

    /// Kill a tracked program by UUID, starting WineBridge if necessary.
    pub async fn kill(&self, id: Uuid) -> Result<()> {
        if self.state()?.program(id).is_none() {
            return Err(BottleError::ProgramNotFound(id).into());
        }
        self.with_bridge(move |bridge| async move { bridge.kill_process(id).await })
            .await
    }

    /// Stop WineBridge, wineserver, and prefix storage.
    pub async fn stop(&self) -> Result<()> {
        let _write = self.0.write_lock.write().await;
        let state = self.state()?;
        Self::stop_state(&state, &self.0.cx).await
    }

    pub fn install(&self, addon: &Addon) -> Operation<()> {
        let addon = addon.clone();
        let bottle = self.clone();
        Operation::new(move |progress, cancellation| async move {
            bottle
                .update(async |state, cx| {
                    let replaced_id = state.replaced_addon_id(&addon);
                    if state
                        .addons
                        .iter()
                        .any(|installed| installed.id() == addon.id())
                    {
                        return Ok(());
                    }
                    let resources = addon.prepare()?;
                    Self::install_item(
                        state,
                        &cx,
                        addon.id(),
                        replaced_id,
                        resources,
                        |state| state.put_addon(addon.clone()),
                        progress,
                        &cancellation,
                    )
                    .await
                })
                .await
        })
    }

    /// Prefix effects completed before a metadata save error are not rolled back.
    pub fn uninstall(&self, id: Uuid) -> Operation<()> {
        let bottle = self.clone();
        Operation::new(move |progress, cancellation| async move {
            bottle
                .update(async |state, cx| {
                    let addon = state
                        .addons
                        .iter()
                        .find(|addon| addon.id() == id)
                        .cloned()
                        .ok_or(BottleError::AddonNotInstalled(id))?;
                    let resources = addon.prepare()?;
                    let winebridge = state.winebridge.path().to_path_buf();
                    let prefix_progress = progress.clone();
                    Self::stop_state(state, &cx).await?;
                    if cancellation.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    state.addons.retain(|installed| installed.id() != id);

                    let runner = state.runner.load().await?;
                    let bottle_path = cx.directories().bottle(state.id);
                    let context = cx.clone();
                    let BottleState {
                        storage,
                        environment,
                        ..
                    } = state;
                    storage
                        .uninstall(
                            &bottle_path,
                            addon.id(),
                            async |prefix, restore_files| {
                                crate::addons::installer::uninstall(
                                    crate::addons::installer::InstallInputs {
                                        prefix,
                                        runner: runner.as_ref(),
                                        winebridge: &winebridge,
                                        environment,
                                    },
                                    &resources,
                                    restore_files,
                                    addon.id(),
                                    &cancellation,
                                    move |_| {
                                        progress.send_replace(Some(Progress::new(Stage::Removing)));
                                    },
                                )
                                .await
                            },
                            &context,
                            &cancellation,
                            move |event| {
                                prefix_progress.send_replace(Some(event));
                            },
                        )
                        .await?;
                    Ok(())
                })
                .await
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn install_item<F>(
        state: &mut BottleState,
        cx: &Context,
        item_id: Uuid,
        replaced_id: Option<Uuid>,
        resources: Vec<InstallResource>,
        update_config: F,
        progress: tokio::sync::watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()>
    where
        F: FnOnce(&mut BottleState),
    {
        Self::stop_state(state, cx).await?;
        update_config(state);
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }

        let runner = state.runner.load().await?;
        let winebridge = state.winebridge.path().to_path_buf();
        let bottle_path = cx.directories().bottle(state.id);
        let context = cx.clone();
        let BottleState {
            storage,
            environment,
            ..
        } = state;
        let step_progress = progress.clone();
        storage
            .install(
                &bottle_path,
                item_id,
                replaced_id,
                async |prefix| {
                    crate::addons::installer::execute(
                        crate::addons::installer::InstallInputs {
                            prefix,
                            runner: runner.as_ref(),
                            winebridge: &winebridge,
                            environment,
                        },
                        &resources,
                        cancellation,
                        move |_| {
                            step_progress.send_replace(Some(Progress::new(Stage::Configuring)));
                        },
                    )
                    .await
                },
                &context,
                cancellation,
                move |event| {
                    progress.send_replace(Some(event));
                },
            )
            .await?;
        crate::addons::installer::replay_environment(environment, &resources);
        Ok(())
    }

    pub fn set_runner(&self, runner: &RunnerComponent) -> Operation<()> {
        let runner = runner.clone();
        let bottle = self.clone();
        Operation::new(move |progress, cancellation| async move {
            bottle
                .update(async |state, cx| {
                    if state.runner.id() == runner.id() {
                        return Ok(());
                    }
                    runner.installed_path()?;
                    if cancellation.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    let installed = state.addons.iter().map(Addon::id).collect::<Vec<_>>();
                    progress.send_replace(Some(Progress::new(Stage::Stopping)));
                    Self::stop_state(state, &cx).await?;
                    if cancellation.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    state.runner = runner;

                    progress.send_replace(Some(Progress::new(Stage::Rebuilding)));
                    let runner = state.runner.load().await?;
                    state
                        .storage
                        .rebuild(
                            runner.as_ref(),
                            &state.runner.id().to_string(),
                            &installed,
                            &cx,
                        )
                        .await?;
                    if cancellation.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    Ok(())
                })
                .await
        })
    }

    pub(super) async fn stop_state(state: &BottleState, cx: &Context) -> Result<()> {
        let bottle_path = cx.directories().bottle(state.id);
        let prefix_path = bottle_path.join("prefix");
        let runner = state.runner.load().await;
        let storage = state.storage.clone();
        let mut first_error = None;
        match WineBridgeClient::try_connect(&prefix_path).await {
            Ok(Some(bridge)) => {
                if let Err(error) = bridge.shutdown().await {
                    first_error.get_or_insert(error);
                }
            }
            Ok(None) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }

        let runner = match runner {
            Ok(runner) => Some(runner),
            Err(error) => {
                first_error.get_or_insert(error);
                None
            }
        };
        if let Some(runner) = runner.as_deref()
            && let Err(error) = shutdown_prefix(runner, &prefix_path).await
        {
            first_error.get_or_insert(error);
        }
        if let Err(error) = storage.stop(&bottle_path, cx).await {
            first_error.get_or_insert(error);
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn with_bridge<T, F, Fut>(&self, work: F) -> Result<T>
    where
        F: FnOnce(WineBridgeClient) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let _read = self.0.write_lock.read().await;
        let state = self.state()?;
        let runner = state.runner.load().await?;
        let bottle_path = self.bottle_path();
        let prefix = self.prefix_path();
        let storage = state.storage.clone();
        let cx = self.0.cx.clone();
        let command = state.wrappers.apply(
            WineBridgeClient::command(runner.as_ref(), &prefix, state.winebridge.path())
                .envs(state.environment.iter()),
        );
        storage.prepare(&bottle_path, &cx).await?;
        work(WineBridgeClient::connect_or_spawn(&prefix, command).await?).await
    }
}
