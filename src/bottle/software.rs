//! Runtime, process, runner, and addon operations on [`Bottle`].

use std::future::Future;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    Context, Operation, Progress, Stage,
    addons::{
        Addon, Component, ComponentKind, Dxvk, IndexEntry, LatencyFlex, Nvapi, Requirement, Runner,
        Slot, Umu, Vkd3d, WineBridge, item::Artifact,
    },
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
    runner::shutdown_prefix,
    winebridge::WineBridgeClient,
};

use super::{
    error::BottleError,
    state::{Bottle, BottleState},
};

fn typed_addon<K: ComponentKind>(entry: &IndexEntry<Component>) -> Addon<K> {
    Addon::try_from(entry).expect("component slot selects its stored addon typestate")
}

fn required_component<K: ComponentKind>(state: &BottleState) -> &Addon<K> {
    state
        .component()
        .expect("required component is always installed")
}

fn component_id(state: &BottleState, slot: Slot) -> Option<Uuid> {
    match slot {
        Slot::Runner => state.component::<Runner>().map(Addon::id),
        Slot::WineBridge => state.component::<WineBridge>().map(Addon::id),
        Slot::Umu => state.component::<Umu>().map(Addon::id),
        Slot::Dxvk => state.component::<Dxvk>().map(Addon::id),
        Slot::Vkd3d => state.component::<Vkd3d>().map(Addon::id),
        Slot::Nvapi => state.component::<Nvapi>().map(Addon::id),
        Slot::LatencyFlex => state.component::<LatencyFlex>().map(Addon::id),
    }
}

fn set_component(state: &mut BottleState, entry: &IndexEntry<Component>) {
    match entry.slot() {
        Slot::Runner => state.runner = typed_addon::<Runner>(entry),
        Slot::WineBridge => state.winebridge = typed_addon::<WineBridge>(entry),
        Slot::Umu => state.umu = Some(typed_addon::<Umu>(entry)),
        Slot::Dxvk => state.dxvk = Some(typed_addon::<Dxvk>(entry)),
        Slot::Vkd3d => state.vkd3d = Some(typed_addon::<Vkd3d>(entry)),
        Slot::Nvapi => state.nvapi = Some(typed_addon::<Nvapi>(entry)),
        Slot::LatencyFlex => state.latency_flex = Some(typed_addon::<LatencyFlex>(entry)),
    }
}

impl Bottle {
    /// Lists DLL overrides configured in this bottle's Wine registry.
    ///
    /// This starts WineBridge if necessary. A missing override registry key is
    /// treated as an empty list, but malformed values are returned as errors.
    /// The result order is unspecified. Reading registry state does not publish
    /// a new [`BottleState`] or notify [`Bottle::watch`](Self::watch).
    ///
    /// # Errors
    ///
    /// Returns an error if the bottle was deleted, its prefix cannot be
    /// prepared, WineBridge cannot start, or the request fails.
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

    /// Sets the Wine loading mode for `dll`.
    ///
    /// This starts WineBridge if necessary and replaces any existing mode for
    /// the same DLL name. The registry change does not publish a new
    /// [`BottleState`] or notify [`Bottle::watch`](Self::watch).
    ///
    /// # Errors
    ///
    /// Returns [`BottleError::DllOverrideModeRequired`] for
    /// [`DllOverrideMode::Unspecified`]. Empty or NUL-containing DLL names are
    /// currently rejected by WineBridge as [`Error::Status`]. Prefix and bridge
    /// failures are also returned.
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

    /// Removes the Wine loading override for `dll`.
    ///
    /// Removing an override that is not present succeeds. This starts
    /// WineBridge if necessary. The registry change does not publish a new
    /// [`BottleState`] or notify [`Bottle::watch`](Self::watch).
    ///
    /// # Errors
    ///
    /// Empty or NUL-containing DLL names are currently rejected by WineBridge
    /// as [`Error::Status`]. Prefix and bridge failures are also returned.
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

    /// Launches a registered program and returns its Windows process ID.
    ///
    /// The program definition is copied before this call waits for shared
    /// bottle access. A concurrent edit therefore does not change or cancel
    /// this launch. WineBridge starts on demand, and the returned ID identifies
    /// the initially launched Windows process. Repeated launches with the same
    /// program UUID share the process group targeted by [`kill`](Self::kill).
    ///
    /// # Errors
    ///
    /// Returns [`BottleError::ProgramNotFound`] if `id` is not registered, or an
    /// error if the prefix cannot be prepared, WineBridge cannot start, or the
    /// process cannot be launched.
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

    /// Returns a snapshot of Windows processes visible in the bottle.
    ///
    /// This includes processes not launched from a registered [`crate::Program`].
    /// WineBridge starts if necessary, and no ordering guarantee is made.
    ///
    /// # Errors
    ///
    /// Returns an error if the prefix cannot be prepared, WineBridge cannot
    /// start, or the process snapshot cannot be read.
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.with_bridge(|bridge| async move { bridge.list_processes().await })
            .await
    }

    /// Terminates the process group associated with a registered program.
    ///
    /// Every running process assigned to the UUID-keyed group is terminated.
    /// This starts WineBridge if necessary; if the program is registered but
    /// has no running group members, the request succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`BottleError::ProgramNotFound`] if `id` is not registered, or an
    /// error if the prefix or bridge operation fails.
    pub async fn kill(&self, id: Uuid) -> Result<()> {
        if self.state()?.program(id).is_none() {
            return Err(BottleError::ProgramNotFound(id).into());
        }
        self.with_bridge(move |bridge| async move { bridge.kill_process(id).await })
            .await
    }

    /// Stops WineBridge, wineserver, and prefix storage.
    ///
    /// This waits for in-flight bridge-backed calls to finish and does not
    /// return until all three cleanup actions have been attempted. Cleanup
    /// continues after a failure and the first error is returned. No
    /// configuration state is changed or published.
    ///
    /// After a successful stop, the next bridge-backed operation applies the
    /// latest environment and wrapper configuration.
    pub async fn stop(&self) -> Result<()> {
        let _write = self.0.write_lock.write().await;
        let state = self.state()?;
        Self::stop_state(&state, &self.0.cx).await
    }

    /// Selects or replaces one downloaded component.
    ///
    /// The operation checks the proposed complete bottle state before mutation.
    /// Switching to Proton selects the newest downloaded UMU when necessary;
    /// switching to Wine removes the unused UMU selection. The current
    /// downloaded component with the supplied UUID is authoritative.
    pub fn set_component(&self, id: Uuid) -> Operation<()> {
        let bottle = self.clone();
        let addons = self.0.addons.clone();
        Operation::new(move |progress, cancellation| async move {
            bottle
                .update(async |state, cx| {
                    let component = addons
                        .component(id)
                        .ok_or(crate::AddonError::NotFound(id))?;
                    if component_id(state, component.slot()) == Some(component.id()) {
                        return Ok(());
                    }

                    let mut candidate = state.clone();
                    let needs_umu = component
                        .requirements()
                        .contains(&Requirement::Slot(Slot::Umu));
                    if needs_umu && candidate.component::<Umu>().is_none() {
                        let umu = addons.latest_component(Slot::Umu).ok_or_else(|| {
                            BottleError::RequiresAddon {
                                required_by: Some(component.id()),
                                requirements: vec![Requirement::Slot(Slot::Umu)],
                            }
                        })?;
                        candidate.umu = Some(typed_addon::<Umu>(&umu));
                    }
                    set_component(&mut candidate, &component);
                    if component.slot() == Slot::Runner && !needs_umu {
                        candidate.umu = None;
                    }
                    candidate.validate_requirements()?;

                    if component.slot().is_runtime() {
                        progress.send_replace(Some(Progress::new(Stage::Stopping)));
                        Self::stop_state(state, &cx).await?;
                        if cancellation.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        let rebuild = component.slot() == Slot::Runner;
                        *state = candidate;
                        if rebuild {
                            progress.send_replace(Some(Progress::new(Stage::Rebuilding)));
                            let installed = [
                                state.component::<Dxvk>().map(Addon::id),
                                state.component::<Vkd3d>().map(Addon::id),
                                state.component::<Nvapi>().map(Addon::id),
                                state.component::<LatencyFlex>().map(Addon::id),
                            ]
                            .into_iter()
                            .flatten()
                            .chain(state.dependencies.iter().map(Addon::id))
                            .collect::<Vec<_>>();
                            let runner = required_component::<Runner>(state)
                                .load_runner(cx.directories(), state.component::<Umu>())
                                .await?;
                            state
                                .storage
                                .rebuild(
                                    runner.as_ref(),
                                    &required_component::<Runner>(state).id().to_string(),
                                    &installed,
                                    &cx,
                                )
                                .await?;
                        }
                        return Ok(());
                    }

                    let replaced_id = component_id(state, component.slot());
                    let resources = vec![component.artifact(cx.directories())];
                    Self::install_item(
                        state,
                        &cx,
                        component.id(),
                        replaced_id,
                        resources,
                        |state| *state = candidate,
                        progress,
                        &cancellation,
                    )
                    .await
                })
                .await
        })
    }

    /// Removes the component occupying `slot`.
    ///
    /// The operation is rejected when removing the component would violate a
    /// bottle or installed-addon requirement. Prefix recipe reversal remains
    /// best effort and does not require the catalog or downloaded files.
    pub fn remove_component(&self, slot: Slot) -> Operation<()> {
        let bottle = self.clone();
        Operation::new(move |progress, cancellation| async move {
            bottle
                .update(async |state, cx| {
                    let mut candidate = state.clone();
                    let (item_id, resource) = match slot {
                        Slot::Runner | Slot::WineBridge => {
                            return Err(BottleError::RequiresAddon {
                                required_by: None,
                                requirements: vec![Requirement::Slot(slot)],
                            }
                            .into());
                        }
                        Slot::Umu => {
                            let addon = candidate
                                .umu
                                .take()
                                .ok_or(BottleError::ComponentNotInstalled(slot))?;
                            (addon.id(), addon.artifact(cx.directories()))
                        }
                        Slot::Dxvk => {
                            let addon = candidate
                                .dxvk
                                .take()
                                .ok_or(BottleError::ComponentNotInstalled(slot))?;
                            (addon.id(), addon.artifact(cx.directories()))
                        }
                        Slot::Vkd3d => {
                            let addon = candidate
                                .vkd3d
                                .take()
                                .ok_or(BottleError::ComponentNotInstalled(slot))?;
                            (addon.id(), addon.artifact(cx.directories()))
                        }
                        Slot::Nvapi => {
                            let addon = candidate
                                .nvapi
                                .take()
                                .ok_or(BottleError::ComponentNotInstalled(slot))?;
                            (addon.id(), addon.artifact(cx.directories()))
                        }
                        Slot::LatencyFlex => {
                            let addon = candidate
                                .latency_flex
                                .take()
                                .ok_or(BottleError::ComponentNotInstalled(slot))?;
                            (addon.id(), addon.artifact(cx.directories()))
                        }
                    };
                    candidate.validate_requirements()?;
                    let resources = vec![resource];
                    let winebridge = required_component::<WineBridge>(state).path(cx.directories());
                    let prefix_progress = progress.clone();
                    Self::stop_state(state, &cx).await?;
                    if cancellation.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    *state = candidate;
                    let runner = required_component::<Runner>(state)
                        .load_runner(cx.directories(), state.component::<Umu>())
                        .await?;
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
                            item_id,
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
                                    item_id,
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
                        .await
                })
                .await
        })
    }

    /// Permanently installs one downloaded dependency into this bottle.
    ///
    /// Reinstalling the same release is idempotent. Dependencies remain
    /// recorded for the bottle's lifetime and cannot be uninstalled separately.
    /// The current downloaded dependency with the supplied UUID is authoritative.
    pub fn install(&self, id: Uuid) -> Operation<()> {
        let bottle = self.clone();
        let addons = self.0.addons.clone();
        Operation::new(move |progress, cancellation| async move {
            bottle
                .update(async |state, cx| {
                    let dependency = addons
                        .dependency(id)
                        .ok_or(crate::AddonError::NotFound(id))?;
                    if state.dependency(dependency.id()).is_some() {
                        return Ok(());
                    }
                    let mut candidate = state.clone();
                    candidate.dependencies.push(dependency.as_ref().into());
                    candidate.validate_requirements()?;
                    let resources = dependency
                        .artifacts()
                        .iter()
                        .map(|artifact| {
                            Artifact::new(
                                dependency.path(cx.directories()).join(&artifact.path),
                                artifact.steps.clone(),
                            )
                        })
                        .collect();
                    Self::install_item(
                        state,
                        &cx,
                        dependency.id(),
                        None,
                        resources,
                        |state| *state = candidate,
                        progress,
                        &cancellation,
                    )
                    .await
                })
                .await
        })
    }

    #[allow(clippy::too_many_arguments)]
    /// Runs the shared, checkpointed addon mutation while the caller holds
    /// exclusive bottle access.
    ///
    /// The draft configuration is updated before prefix work but is persisted
    /// only by [`Bottle::update`] after this returns. Virgo can satisfy an
    /// installation from a cached layer without executing the recipe, so its
    /// environment steps are replayed into the draft to produce the same
    /// persisted configuration as a fresh installation.
    async fn install_item<F>(
        state: &mut BottleState,
        cx: &Context,
        item_id: Uuid,
        replaced_id: Option<Uuid>,
        resources: Vec<Artifact>,
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

        let runner = required_component::<Runner>(state)
            .load_runner(cx.directories(), state.component::<Umu>())
            .await?;
        let winebridge = required_component::<WineBridge>(state).path(cx.directories());
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

    /// Performs uncancellable lifecycle cleanup while exclusive bottle access
    /// prevents new bridge work.
    ///
    /// WineBridge shutdown, runner shutdown, and storage unmount are all
    /// attempted; the first error is retained.
    pub(super) async fn stop_state(state: &BottleState, cx: &Context) -> Result<()> {
        let bottle_path = cx.directories().bottle(state.id);
        let prefix_path = bottle_path.join("prefix");
        let runner = required_component::<Runner>(state)
            .load_runner(cx.directories(), state.component::<Umu>())
            .await;
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

    /// Holds shared bottle access while preparing the persisted prefix and
    /// performing one WineBridge request.
    ///
    /// Environment and wrappers come from one published state snapshot, and
    /// WineBridge remains running afterward. Shared access permits concurrent
    /// requests but currently does not coalesce simultaneous first starts.
    async fn with_bridge<T, F, Fut>(&self, work: F) -> Result<T>
    where
        F: FnOnce(WineBridgeClient) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let _read = self.0.write_lock.read().await;
        let state = self.state()?;
        let runner = required_component::<Runner>(&state)
            .load_runner(self.0.cx.directories(), state.component::<Umu>())
            .await?;
        let bottle_path = self.bottle_path();
        let prefix = self.prefix_path();
        let storage = state.storage.clone();
        let cx = self.0.cx.clone();
        let command = state.wrappers.apply(
            WineBridgeClient::command(
                runner.as_ref(),
                &prefix,
                required_component::<WineBridge>(&state).path(self.0.cx.directories()),
            )
            .envs(state.environment.iter()),
        );
        storage.prepare(&bottle_path, &cx).await?;
        work(WineBridgeClient::connect_or_spawn(&prefix, command).await?).await
    }
}
