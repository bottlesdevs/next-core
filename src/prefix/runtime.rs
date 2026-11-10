//! Starts, inspects, and stops a prefix's Wine runtime.
//!
//! Runtime operations share the prefix control lock with edits, deletion,
//! and history changes. `WineBridge` is attached when already running or started
//! from the currently published configuration. Virgo mounts are created before
//! startup and released only after Wine processes stop.

use super::{Inner, PrefixState};
use crate::runner::Runner;
use crate::{
    Job, Operation, ProgramSpec, Stage,
    error::Result,
    proto::{DllOverride, DllOverrideMode, Process},
    winebridge::WineBridgeClient,
};
use futures_core::Stream;
use futures_util::{
    StreamExt, future,
    stream::{self, BoxStream},
};
use std::{future::Future, path::Path, sync::Arc};
use tokio_stream::wrappers::WatchStream;
use uuid::Uuid;

impl Inner {
    /// Launches `launch` through `WineBridge` in process group `group`.
    ///
    /// Once startup succeeds, a rejected process launch leaves the runtime running.
    ///
    /// # Errors
    ///
    /// Returns an error if startup or attachment fails, the operation is
    /// cancelled, or `WineBridge` rejects the launch.
    pub(crate) fn launch(self: &Arc<Self>, group: Uuid, launch: ProgramSpec) -> Operation<u32> {
        let prefix = self.clone();
        Operation::new(move |job| async move {
            let _control = job.cancellable(prefix.control.lock()).await?;
            let state = prefix.state()?;
            let bridge = prefix.attach_or_start(&state, &job).await?;
            bridge
                .launch_process(
                    group,
                    launch.executable().into(),
                    launch.args().to_vec(),
                    launch.working_directory().map(str::to_owned),
                    launch.new_console(),
                )
                .await
        })
    }

    /// Lists the processes in `group`, or every process in the prefix, returning
    /// an empty list when the runtime is stopped.
    ///
    /// # Errors
    ///
    /// Returns an error if the prefix was deleted, discovery fails, or the
    /// connected `WineBridge` cannot list its processes.
    pub(crate) async fn processes(&self, group: Option<Uuid>) -> Result<Vec<Process>> {
        let _control = self.control.lock().await;
        self.state()?;
        match self.bridge().await? {
            Some(bridge) => bridge.list_processes(group).await,
            None => Ok(Vec::new()),
        }
    }

    /// Streams the launched groups that have a running process, or `None` while
    /// Wine is not running.
    ///
    /// The first item is the current state. The stream attaches to a running
    /// `WineBridge` but never starts one; after Wine stops it yields `None` and
    /// resumes when this prefix next starts `WineBridge`. It ends once the prefix
    /// is deleted.
    pub(crate) fn watch_groups(
        self: &Arc<Self>,
    ) -> impl Stream<Item = Option<Vec<Uuid>>> + Send + 'static + use<> {
        let prefix = self.clone();
        let mut state = self.state.subscribe();
        let deleted = async move {
            let _ = state.wait_for(Option::is_none).await;
        };
        // Attach now and again whenever this prefix starts `WineBridge`.
        WatchStream::new(self.bridge_started.subscribe())
            .take_until(deleted)
            .then(move |_| prefix.clone().attach())
            .flat_map(|run| match run {
                Some(groups) => groups
                    .map(Some)
                    .chain(stream::once(future::ready(None)))
                    .boxed(),
                None => stream::once(future::ready(None)).boxed(),
            })
    }

    /// Connects to this prefix's running `WineBridge`, if there is one.
    ///
    /// # Errors
    ///
    /// Returns an error if discovery fails or a discovered `WineBridge` is unreachable.
    pub(super) async fn bridge(&self) -> Result<Option<WineBridgeClient>> {
        WineBridgeClient::try_connect(&self.root.join("prefix")).await
    }

    /// Streams a reachable `WineBridge`'s running groups until its stream ends or fails.
    async fn attach(self: Arc<Self>) -> Option<BoxStream<'static, Vec<Uuid>>> {
        let bridge = self.bridge().await.ok()??;
        let events = bridge.watch_groups().await.ok()?;
        Some(
            events
                .take_while(|running| future::ready(running.is_ok()))
                .filter_map(|running| future::ready(running.ok()))
                .map(|running| {
                    running
                        .ids
                        .iter()
                        .filter_map(|id| id.parse().ok())
                        .collect()
                })
                .boxed(),
        )
    }

    /// Kills process group `group` when `WineBridge` is currently running.
    ///
    /// A stopped runtime is a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if discovery fails or `WineBridge` cannot terminate the
    /// process group.
    pub(crate) async fn kill(&self, group: Uuid) -> Result<()> {
        let _control = self.control.lock().await;
        self.state()?;
        if let Some(bridge) = self.bridge().await? {
            bridge.kill_process(group).await?;
        }
        Ok(())
    }

    /// Stops Wine and releases backend storage under the prefix lock.
    ///
    /// Storage release is attempted only after Wine shutdown succeeds. A shutdown
    /// error can therefore leave mounts and discovery files in place.
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
    /// Storage release is attempted only after Wine shutdown succeeds. A shutdown
    /// error can therefore leave mounts and discovery files in place.
    ///
    /// # Errors
    ///
    /// Returns an error if state loading, runner resolution, process shutdown,
    /// unmounting, or discovery cleanup fails.
    pub(crate) async fn stop_locked(&self) -> Result<()> {
        let state = self.state()?;
        let wineprefix = self.root.join("prefix");
        if crate::utils::fs::exists(&wineprefix).await? {
            let runner = state.runner.load_runner(state.umu.as_ref()).await?;
            stop(runner.as_ref(), &wineprefix).await?;
        }
        self.storage.release(&self.root).await
    }

    /// Returns an operation that starts or attaches `WineBridge` and lists DLL overrides.
    pub(crate) fn dll_overrides(self: &Arc<Self>) -> Operation<Vec<DllOverride>> {
        self.with_bridge(async |bridge| bridge.list_dll_overrides().await)
    }

    /// Returns an operation that sets one Wine DLL override through `WineBridge`.
    pub(crate) fn set_dll_override(
        self: &Arc<Self>,
        dll: String,
        mode: DllOverrideMode,
    ) -> Operation<()> {
        self.with_bridge(async move |bridge| bridge.set_dll_override(dll, mode).await)
    }

    /// Returns an operation that removes one Wine DLL override through `WineBridge`.
    pub(crate) fn unset_dll_override(self: &Arc<Self>, dll: String) -> Operation<()> {
        self.with_bridge(async move |bridge| bridge.delete_dll_override(dll).await)
    }

    /// Runs one `WineBridge` request while holding prefix coordination.
    fn with_bridge<R, Fut>(
        self: &Arc<Self>,
        work: impl FnOnce(WineBridgeClient) -> Fut + Send + 'static,
    ) -> Operation<R>
    where
        R: Send + 'static,
        Fut: Future<Output = Result<R>> + Send + 'static,
    {
        let prefix = self.clone();
        Operation::new(move |job| async move {
            let _control = job.cancellable(prefix.control.lock()).await?;
            let state = prefix.state()?;
            let bridge = prefix.attach_or_start(&state, &job).await?;
            work(bridge).await
        })
    }

    /// Reuses a discovered `WineBridge` or prepares storage and starts a new one.
    ///
    /// On failed startup, the method stops any spawned Wine processes and
    /// releases storage. A cleanup failure takes precedence over the startup or
    /// cancellation error that triggered cleanup.
    ///
    /// # Errors
    ///
    /// Returns cancellation, discovery, runner, mount, process, connection, or
    /// cleanup errors encountered while obtaining a usable client.
    async fn attach_or_start(&self, state: &PrefixState, job: &Job) -> Result<WineBridgeClient> {
        job.transition(Stage::Preparing)?;
        if let Some(bridge) = self.bridge().await? {
            return Ok(bridge);
        }
        let wineprefix = self.root.join("prefix");
        let vars = state.effective_env_vars();
        self.stop_locked().await?;
        let runner = state.runner.load_runner(state.umu.as_ref()).await?;
        let mounted = self.storage.mount(state, &self.root).await;
        if mounted.is_err() {
            self.storage.release(&self.root).await?;
        }
        mounted?;
        if let Err(error) = job.check_cancellation() {
            self.storage.release(&self.root).await?;
            return Err(error);
        }
        let command = state.wrappers.apply(WineBridgeClient::command(
            runner.as_ref(),
            &wineprefix,
            state.winebridge.path(),
            vars.iter(),
        ));
        let result = WineBridgeClient::connect_or_spawn(&wineprefix, command)
            .await
            .and_then(|bridge| job.check_cancellation().map(|()| bridge));
        if result.is_err() {
            stop(runner.as_ref(), &wineprefix).await?;
            self.storage.release(&self.root).await?;
        } else {
            self.bridge_started.send_replace(());
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
/// Returns an error from `wineboot`, `WineBridge` shutdown, `wineserver`, or
/// discovery cleanup.
pub(super) async fn initialize(runner: &dyn Runner, wineprefix: &Path) -> Result<()> {
    let initialized = runner.wineboot(wineprefix, "--init").await;
    stop(runner, wineprefix).await?;
    initialized
}

/// Stops `WineBridge`, kills and drains wineserver, then clears discovery files.
///
/// Failure to request `WineBridge` shutdown is logged and does not prevent the
/// wineserver fallback. The caller remains responsible for backend storage.
///
/// # Errors
///
/// Returns an error if either wineserver command or discovery cleanup fails.
pub(super) async fn stop(runner: &dyn Runner, wineprefix: &Path) -> Result<()> {
    if let Err(error) = WineBridgeClient::shutdown_existing(wineprefix).await {
        tracing::debug!(%error, "WineBridge shutdown failed; stopping wineserver");
    }
    for argument in ["-k", "-w"] {
        runner.wineserver(wineprefix, argument).await?;
    }
    WineBridgeClient::clear_discovery(wineprefix).await?;
    Ok(())
}
