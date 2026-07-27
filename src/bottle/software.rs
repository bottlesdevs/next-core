use std::future::Future;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    Context, Operation,
    compatibility::{
        components::{Component, catalog::ComponentKind},
        dependencies::Dependency,
        installer::{InstallProgress, InstallResource, Installable, UninstallProgress},
    },
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
    runner::{Runner, RunnerKind, shutdown_prefix},
    winebridge::WineBridgeClient,
};

use super::{
    error::BottleError,
    state::{Bottle, BottleState, RunnerSelection},
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
        let _write = self.0.write.lock().await;
        let state = self.state()?;
        Self::stop_state(&state, &self.0.cx).await
    }

    pub fn install_component(&self, component: &Component) -> Operation<(), InstallProgress> {
        let component = component.clone();
        let bottle = self.clone();
        self.0.cx.spawn(move |progress, cancellation| async move {
            let kind = component.kind();
            if !matches!(
                kind,
                ComponentKind::Dxvk
                    | ComponentKind::Vkd3d
                    | ComponentKind::Nvapi
                    | ComponentKind::LatencyFlex
            ) {
                return Err(BottleError::InvalidPrefixComponent.into());
            }
            bottle
                .update(async |state, cx| {
                    let replaced_id = match kind {
                        ComponentKind::Dxvk => state.dxvk.as_ref(),
                        ComponentKind::Vkd3d => state.vkd3d.as_ref(),
                        ComponentKind::Nvapi => state.nvapi.as_ref(),
                        ComponentKind::LatencyFlex => state.latency_flex.as_ref(),
                        _ => unreachable!(),
                    }
                    .map(Component::id);
                    if replaced_id == Some(component.id()) {
                        return Ok(());
                    }
                    let resources = component.prepare(cx.directories())?;
                    Self::install_item(
                        state,
                        &cx,
                        component.id(),
                        replaced_id,
                        resources,
                        |state| {
                            *match kind {
                                ComponentKind::Dxvk => &mut state.dxvk,
                                ComponentKind::Vkd3d => &mut state.vkd3d,
                                ComponentKind::Nvapi => &mut state.nvapi,
                                ComponentKind::LatencyFlex => &mut state.latency_flex,
                                _ => unreachable!(),
                            } = Some(component.clone());
                        },
                        progress,
                        &cancellation,
                    )
                    .await
                })
                .await
        })
    }

    pub fn install_dependency(&self, dependency: &Dependency) -> Operation<(), InstallProgress> {
        let dependency = dependency.clone();
        let bottle = self.clone();
        self.0.cx.spawn(move |progress, cancellation| async move {
            bottle
                .update(async |state, cx| {
                    if state
                        .dependencies
                        .iter()
                        .any(|installed| installed.id() == dependency.id())
                    {
                        return Ok(());
                    }
                    let resources = dependency.prepare(cx.directories())?;
                    Self::install_item(
                        state,
                        &cx,
                        dependency.id(),
                        None,
                        resources,
                        |state| state.dependencies.push(dependency.clone()),
                        progress,
                        &cancellation,
                    )
                    .await
                })
                .await
        })
    }

    /// Prefix effects completed before a metadata save error are not rolled back.
    pub fn uninstall_component(&self, id: Uuid) -> Operation<Component, UninstallProgress> {
        let bottle = self.clone();
        self.0.cx.spawn(move |progress, cancellation| async move {
            bottle
                .update(async |state, cx| {
                    let component = [
                        state.dxvk.as_ref(),
                        state.vkd3d.as_ref(),
                        state.nvapi.as_ref(),
                        state.latency_flex.as_ref(),
                    ]
                    .into_iter()
                    .flatten()
                    .find(|component| component.id() == id)
                    .cloned()
                    .ok_or(BottleError::ComponentNotInstalled(id))?;
                    let resources = component.prepare(cx.directories())?;
                    let winebridge = state.winebridge.path().to_path_buf();
                    let prefix_progress = progress.clone();
                    Self::stop_state(state, &cx).await?;
                    if cancellation.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    match component.kind() {
                        ComponentKind::Dxvk => state.dxvk = None,
                        ComponentKind::Vkd3d => state.vkd3d = None,
                        ComponentKind::Nvapi => state.nvapi = None,
                        ComponentKind::LatencyFlex => state.latency_flex = None,
                        _ => unreachable!(),
                    }

                    let runner = Self::load_runner(state)?;
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
                            component.id(),
                            async |prefix, restore_files| {
                                crate::compatibility::installer::uninstall(
                                    crate::compatibility::installer::InstallInputs {
                                        prefix,
                                        runner: runner.as_ref(),
                                        winebridge: &winebridge,
                                        environment,
                                    },
                                    &resources,
                                    restore_files,
                                    component.id(),
                                    &cancellation,
                                    move |step| {
                                        progress.send_replace(Some(step.into()));
                                    },
                                )
                                .await
                            },
                            &context,
                            &cancellation,
                            move |event| {
                                prefix_progress.send_replace(Some(event.into()));
                            },
                        )
                        .await?;
                    Ok(component)
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
        progress: tokio::sync::watch::Sender<Option<InstallProgress>>,
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

        let runner = Self::load_runner(state)?;
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
                    crate::compatibility::installer::execute(
                        &context,
                        crate::compatibility::installer::InstallInputs {
                            prefix,
                            runner: runner.as_ref(),
                            winebridge: &winebridge,
                            environment,
                        },
                        &resources,
                        cancellation,
                        move |step| {
                            step_progress.send_replace(Some(step.into()));
                        },
                    )
                    .await
                },
                &context,
                cancellation,
                move |event| {
                    progress.send_replace(Some(event.into()));
                },
            )
            .await?;
        crate::compatibility::installer::replay_environment(environment, &resources);
        Ok(())
    }

    pub async fn install_runner(
        &self,
        component: &Component,
        umu: Option<&Component>,
    ) -> Result<()> {
        let selection = match component.kind().runner_kind() {
            Some(RunnerKind::Wine) if umu.is_none() => RunnerSelection::wine(component.clone())?,
            Some(RunnerKind::Proton) => RunnerSelection::proton(
                component.clone(),
                umu.cloned().ok_or(BottleError::ProtonRunnerWithoutUmu)?,
            )?,
            Some(RunnerKind::Wine) => return Err(BottleError::WineRunnerWithUmu.into()),
            None => return Err(BottleError::RunnerComponentRequired.into()),
        };
        self.update(async |state, cx| {
            if state.runner == selection {
                return Ok(());
            }
            let installed = [
                state.dxvk.as_ref(),
                state.vkd3d.as_ref(),
                state.nvapi.as_ref(),
                state.latency_flex.as_ref(),
            ]
            .into_iter()
            .flatten()
            .map(Component::id)
            .chain(state.dependencies.iter().map(|dependency| dependency.id()))
            .collect::<Vec<_>>();
            Self::stop_state(state, &cx).await?;
            state.runner = selection;

            let runner = Self::load_runner(state)?;
            state
                .storage
                .rebuild(
                    runner.as_ref(),
                    &state.runner.runner().id().to_string(),
                    &installed,
                    &cx,
                )
                .await
        })
        .await
    }

    fn load_runner(state: &BottleState) -> Result<Box<dyn Runner>> {
        state.runner.validate()?;
        crate::runner::load_runner(
            state.runner.runner().path(),
            state.runner.kind(),
            state.runner.umu().map(Component::path),
        )
    }

    async fn stop_state(state: &BottleState, cx: &Context) -> Result<()> {
        let bottle_path = cx.directories().bottle(state.id);
        let prefix_path = bottle_path.join("prefix");
        let runner = Self::load_runner(state);
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
        T: Send + 'static,
        F: FnOnce(WineBridgeClient) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let state = self.state()?;
        let runner = Self::load_runner(&state)?;
        let bottle_path = self.bottle_path();
        let prefix = self.prefix_path();
        let storage = state.storage.clone();
        let cx = self.0.cx.clone();
        let command = state.wrappers.apply(
            WineBridgeClient::command(runner.as_ref(), &prefix, state.winebridge.path())
                .envs(state.environment.iter()),
        );
        let operation: Operation<_, ()> = self.0.cx.spawn(move |_, _| async move {
            storage.prepare(&bottle_path, &cx).await?;
            work(WineBridgeClient::connect_or_spawn(&prefix, command).await?).await
        });
        operation.await
    }
}
