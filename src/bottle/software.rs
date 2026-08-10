//! Runtime, process, runner, and addon operations on [`Bottle`].

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

    /// Installs an addon into this bottle.
    ///
    /// The operation captures an owned copy of `addon`; later catalog refreshes
    /// do not alter it. When prefix work is required, the addon must record
    /// downloaded resources whose paths still exist when the operation runs.
    ///
    /// For Standard storage, installing a slotted addon runs the new recipe and
    /// replaces the recorded occupant without first uninstalling the old
    /// recipe. For Virgo, the old layer UUID is replaced by the new layer, which
    /// is created if it is not cached. Addons without slots coexist.
    ///
    /// Installation stops the bottle and uses an FVS checkpoint under both
    /// storage strategies. If work fails, or cancellation is observed while
    /// the operation remains polled, checkpoint restoration is attempted.
    /// Restore failure is logged and the original error is returned, so the
    /// prefix may remain partially modified. A metadata-save failure after
    /// successful prefix work is not rolled back.
    ///
    /// If the same addon UUID is already recorded, stop and prefix work are
    /// skipped. The current metadata is still persisted, so this path can
    /// return a persistence error.
    ///
    /// # Errors
    ///
    /// Returns an error if the captured addon is not downloaded or its recorded
    /// resources are unavailable. Prefix, FVS, installer, cancellation, and
    /// persistence failures are also returned.
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

    /// Uninstalls the addon identified by `id` from this bottle.
    ///
    /// Uninstallation stops the bottle and uses an FVS checkpoint under both
    /// storage strategies. If work fails, or cancellation is observed while
    /// the operation remains polled, checkpoint restoration is attempted.
    /// Restore failure is logged and the original error is returned, so the
    /// prefix may remain partially modified. A metadata-save failure after
    /// successful prefix work is not rolled back.
    ///
    /// Recipe removal is best-effort and not every installation step has an
    /// inverse. Some cleanup failures are logged and ignored, so success means
    /// the addon was removed from bottle metadata; files, registry values, or
    /// other prefix effects may remain.
    ///
    /// # Errors
    ///
    /// Returns [`BottleError::AddonNotInstalled`] if `id` is not recorded as
    /// installed. The operation also returns component, prefix, installer,
    /// cancellation, and persistence failures.
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

    /// Changes the runner used by this bottle.
    ///
    /// The operation captures an owned copy of `runner`; later catalog
    /// refreshes do not alter it. Runner UUID defines identity. If it matches
    /// the recorded runner, differing metadata or paths in the supplied
    /// snapshot are ignored and prefix work is skipped, although the current
    /// bottle metadata is still persisted.
    ///
    /// Otherwise the new runner must record an installed path. The bottle is
    /// stopped and remains stopped. Standard storage changes only the recorded
    /// runner; Virgo rebuilds its base and addon layer list. Runner layout and
    /// Proton/UMU requirements are revalidated during loading.
    ///
    /// # Errors
    ///
    /// Returns an error if the runner is unavailable or invalid, the bottle
    /// cannot be stopped or rebuilt, cancellation is requested, or the updated
    /// metadata cannot be persisted. Failure before persistence leaves the old
    /// published state in place but may leave the bottle stopped.
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

    /// Performs uncancellable lifecycle cleanup while exclusive bottle access
    /// prevents new bridge work.
    ///
    /// WineBridge shutdown, runner shutdown, and storage unmount are all
    /// attempted; the first error is retained.
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
