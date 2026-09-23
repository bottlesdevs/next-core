//! Starts, inspects, and stops an environment's Wine runtime.
//!
//! Runtime operations share the environment control lock with edits, deletion,
//! and history changes. WineBridge is attached when already running or started
//! from the currently published configuration. Virgo mounts are created before
//! startup and released only after Wine processes stop.

use super::{BackendSource, Environment, State};
use crate::runner::Runner;
use crate::{
    Operation, PrefixBackend, ProgramSpec, Progress, Stage,
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
    winebridge::WineBridgeClient,
};
use std::{future::Future, path::Path, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

impl<T: BackendSource> Environment<T>
where
    State<T>: next_config::Config + Clone + PartialEq + Send + Sync,
{
    /// Resolves a program from the locked state and launches it through WineBridge.
    ///
    /// # Errors
    ///
    /// Returns an error if selection fails, startup or attachment fails, the
    /// operation is cancelled, or WineBridge rejects the launch.
    pub(crate) fn launch(
        self: &Arc<Self>,
        select: impl FnOnce(&State<T>) -> Result<(Uuid, ProgramSpec)> + Send + 'static,
    ) -> Operation<u32> {
        let environment = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let _control = environment.lock_control(&cancellation).await?;
            let state = environment.state()?;
            let (id, launch) = select(&state)?;
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

    /// Lists WineBridge processes, returning an empty list when the runtime is stopped.
    ///
    /// # Errors
    ///
    /// Returns an error if the environment was deleted, discovery fails, or the
    /// connected WineBridge cannot list its processes.
    pub(crate) async fn processes(&self) -> Result<Vec<Process>> {
        let _control = self.control.lock().await;
        self.state()?;
        match WineBridgeClient::try_connect(&self.root.join("prefix")).await? {
            Some(bridge) => bridge.list_processes().await,
            None => Ok(Vec::new()),
        }
    }

    /// Resolves and kills a process when WineBridge is currently running.
    ///
    /// A stopped runtime is a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if selection or discovery fails, or WineBridge cannot
    /// terminate the selected process.
    pub(crate) async fn kill(&self, select: impl FnOnce(&State<T>) -> Result<Uuid>) -> Result<()> {
        let _control = self.control.lock().await;
        let state = self.state()?;
        let id = select(&state)?;
        if let Some(bridge) = WineBridgeClient::try_connect(&self.root.join("prefix")).await? {
            bridge.kill_process(id).await?;
        }
        Ok(())
    }

    /// Stops Wine and releases backend storage under the environment lock.
    ///
    /// # Errors
    ///
    /// Returns an error if state loading, runner resolution, process shutdown,
    /// unmounting, or discovery cleanup fails.
    pub(crate) async fn stop(&self) -> Result<()> {
        let _control = self.control.lock().await;
        self.stop_locked().await
    }

    /// Stops Wine and releases storage while the caller retains coordination.
    ///
    /// # Errors
    ///
    /// Returns an error if state loading, runner resolution, process shutdown,
    /// unmounting, or discovery cleanup fails.
    pub(crate) async fn stop_locked(&self) -> Result<()> {
        let state = self.state()?;
        let prefix = self.root.join("prefix");
        if crate::utils::fs::exists(&prefix).await? {
            let config = &state.config;
            let runner = config
                .runner()
                .load_runner(self.context.directories(), config.umu())
                .await?;
            stop(runner.as_ref(), &prefix).await?;
        }
        self.release_storage(state.data.backend()).await
    }

    /// Releases mounts and discovery files after Wine has stopped.
    ///
    /// # Errors
    ///
    /// Returns an error if a Virgo mount cannot be released or discovery files
    /// cannot be cleared.
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

    /// Returns an operation that starts or attaches WineBridge and lists DLL overrides.
    pub(crate) fn dll_overrides(self: &Arc<Self>) -> Operation<Vec<DllOverride>> {
        self.with_bridge(async |bridge| bridge.list_dll_overrides().await)
    }

    /// Returns an operation that sets one Wine DLL override through WineBridge.
    pub(crate) fn set_dll_override(
        self: &Arc<Self>,
        dll: String,
        mode: DllOverrideMode,
    ) -> Operation<()> {
        self.with_bridge(async move |bridge| bridge.set_dll_override(dll, mode).await)
    }

    /// Returns an operation that removes one Wine DLL override through WineBridge.
    pub(crate) fn unset_dll_override(self: &Arc<Self>, dll: String) -> Operation<()> {
        self.with_bridge(async move |bridge| bridge.delete_dll_override(dll).await)
    }

    /// Runs one WineBridge request while holding environment coordination.
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

    /// Reuses a discovered WineBridge or prepares storage and starts a new one.
    ///
    /// On failed startup, the method stops any spawned Wine processes and
    /// releases storage before returning the original error.
    ///
    /// # Errors
    ///
    /// Returns cancellation, discovery, runner, mount, process, connection, or
    /// cleanup errors encountered while obtaining a usable client.
    async fn attach_or_start(
        &self,
        state: &State<T>,
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
        let config = &state.config;
        let backend = state.data.backend();
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
            stop(runner.as_ref(), &prefix).await?;
            self.release_storage(backend).await?;
        }
        result
    }
}

/// Initializes a prefix and then stops all Wine processes.
///
/// Shutdown is attempted even when `wineboot --init` fails; a shutdown failure
/// takes precedence over the initialization result.
///
/// # Errors
///
/// Returns an error from `wineboot`, WineBridge shutdown, `wineserver`, or
/// discovery cleanup.
pub(super) async fn initialize(runner: &dyn Runner, prefix: &Path) -> Result<()> {
    let initialized = runner.wineboot(prefix, "--init").await;
    stop(runner, prefix).await?;
    initialized
}

/// Stops WineBridge, kills and drains wineserver, then clears discovery files.
///
/// Failure to request WineBridge shutdown is logged and does not prevent the
/// wineserver fallback. The caller remains responsible for backend storage.
///
/// # Errors
///
/// Returns an error if either wineserver command or discovery cleanup fails.
pub(super) async fn stop(runner: &dyn Runner, prefix: &Path) -> Result<()> {
    if let Err(error) = WineBridgeClient::shutdown_existing(prefix).await {
        tracing::debug!(%error, "WineBridge shutdown failed; stopping wineserver");
    }
    for argument in ["-k", "-w"] {
        runner.wineserver(prefix, argument).await?;
    }
    WineBridgeClient::clear_discovery(prefix).await?;
    Ok(())
}
