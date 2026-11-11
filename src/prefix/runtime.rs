//! Starts, inspects, and stops a prefix's Wine runtime.
//!
//! Runtime operations share the prefix control lock with edits, deletion,
//! and history changes. `WineBridge` is attached when already running or started
//! from the currently published configuration. Virgo mounts are created before
//! startup and released only after Wine processes stop.

use super::{Prefix, PrefixState};
use crate::runner::Runner;
use crate::{
    Job, Operation, ProgramSpec, Stage,
    error::{Result, ResultExt},
    proto::{DllOverride, DllOverrideMode, Process},
    winebridge::WineBridgeClient,
};
use futures_core::Stream;
use futures_util::{
    StreamExt, future,
    stream::{self, BoxStream},
};
use std::{future::Future, path::Path};
use tokio_stream::wrappers::WatchStream;
use uuid::Uuid;

impl Prefix {
    /// Creates an operation that launches `program` in process group `group`.
    ///
    /// `group` is the `WineBridge` process-group identifier chosen by the
    /// caller; it is not persisted. Success returns the new process ID; it does
    /// not wait for the process to exit. Cancellation does not interrupt an
    /// in-flight launch request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation may return prefix, runner, storage,
    /// cancellation, or `WineBridge` errors while starting the process.
    #[tracing::instrument(
        skip_all,
        fields(prefix = %self.id(), %group, executable = program.executable())
    )]
    pub fn launch(&self, group: Uuid, program: ProgramSpec) -> Operation<u32> {
        let prefix = self.clone();
        Operation::new(move |job| async move {
            let _control = job.cancellable(prefix.0.control.lock()).await?;
            let state = prefix.state()?;
            let bridge = prefix.attach_or_start(&state, &job).await?;
            bridge
                .launch_process(
                    group,
                    program.executable().into(),
                    program.args().to_vec(),
                    program.working_directory().map(str::to_owned),
                    program.new_console(),
                )
                .await
                .inspect(|pid| tracing::info!(pid, "program launched"))
        })
    }

    /// Lists the processes in process group `group`, or every process in the
    /// prefix when `group` is `None`.
    ///
    /// A group's processes include the children of the programs launched into
    /// it. A stopped prefix returns an empty vector and is not started.
    ///
    /// # Errors
    ///
    /// Returns an error if the prefix was deleted, discovery fails, or the
    /// connected `WineBridge` cannot list its processes.
    pub async fn processes(&self, group: Option<Uuid>) -> Result<Vec<Process>> {
        let _control = self.0.control.lock().await;
        self.state()?;
        match self.bridge().await? {
            Some(bridge) => bridge.list_processes(group).await,
            None => Ok(Vec::new()),
        }
    }

    /// Streams the process groups that have a running process, or `None` while
    /// Wine is not running.
    ///
    /// The first item is the current state; later items follow every change.
    /// Use [`Self::processes`] for the processes in a group. Watching never
    /// starts Wine. After Wine stops, the stream yields `None` and resumes when
    /// this prefix next starts it. The stream ends once the prefix is deleted.
    pub fn watch_groups(&self) -> impl Stream<Item = Option<Vec<Uuid>>> + Send + 'static + use<> {
        let prefix = self.clone();
        let mut state = self.0.state.subscribe();
        let deleted = async move {
            let _ = state.wait_for(Option::is_none).await;
        };
        // Attach now and again whenever this prefix starts `WineBridge`.
        WatchStream::new(self.0.bridge_started.subscribe())
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
        WineBridgeClient::try_connect(&self.0.root.join("prefix")).await
    }

    /// Streams a reachable `WineBridge`'s running groups until its stream ends or fails.
    async fn attach(self) -> Option<BoxStream<'static, Vec<Uuid>>> {
        let bridge = self.bridge().await.log_info()??;
        let events = bridge.watch_groups().await.log_info()?;
        Some(
            events
                .map(|running| running.log_debug())
                .take_while(|running| future::ready(running.is_some()))
                .filter_map(future::ready)
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

    /// Terminates a `WineBridge` process group without starting a stopped runtime.
    ///
    /// A stopped runtime is a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the prefix was deleted, discovery fails, or
    /// `WineBridge` cannot terminate the process group.
    #[tracing::instrument(skip_all, fields(prefix = %self.id(), %group))]
    pub async fn kill(&self, group: Uuid) -> Result<()> {
        let _control = self.0.control.lock().await;
        self.state()?;
        if let Some(bridge) = self.bridge().await? {
            bridge.kill_process(group).await?;
        }
        Ok(())
    }

    /// Stops Wine and releases storage mounted for the prefix.
    ///
    /// Calling this for an already stopped prefix is safe.
    ///
    /// # Errors
    ///
    /// Returns an error if state or runner resolution, Wine shutdown, storage
    /// release, or discovery cleanup fails.
    #[tracing::instrument(skip_all, fields(prefix = %self.id()))]
    pub async fn stop(&self) -> Result<()> {
        let _control = self.0.control.lock().await;
        self.stop_locked().await?;
        tracing::info!("prefix stopped");
        Ok(())
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
        let wineprefix = self.0.root.join("prefix");
        if crate::utils::fs::exists(&wineprefix).await? {
            let runner = state.runner.runner(state.umu.as_ref())?;
            stop(runner.as_ref(), &wineprefix).await?;
        }
        self.0.storage.release(&self.0.root).await
    }

    /// Creates an operation that lists configured DLL overrides.
    ///
    /// The operation starts or reconnects to `WineBridge` if necessary and
    /// leaves the runtime running. Cancellation does not interrupt an in-flight
    /// `WineBridge` request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during prefix access, runner or
    /// `WineBridge` startup, cancellation, or the `WineBridge` request.
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.with_bridge(async |bridge| bridge.list_dll_overrides().await)
    }

    /// Creates an operation that sets one Wine DLL override.
    ///
    /// A stopped runtime is started and left running. Cancellation does not
    /// interrupt an in-flight `WineBridge` request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during prefix access, runner or
    /// `WineBridge` startup, cancellation, or the `WineBridge` request.
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        let dll = dll.into();
        self.with_bridge(async move |bridge| bridge.set_dll_override(dll, mode).await)
    }

    /// Creates an operation that removes one Wine DLL override.
    ///
    /// A stopped runtime is started and left running. A missing override is a
    /// successful no-op. Cancellation does not interrupt an in-flight
    /// `WineBridge` request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during prefix access, runner or
    /// `WineBridge` startup, cancellation, or the `WineBridge` request.
    pub fn unset_dll_override(&self, dll: impl Into<String>) -> Operation<()> {
        let dll = dll.into();
        self.with_bridge(async move |bridge| bridge.delete_dll_override(dll).await)
    }

    /// Runs one `WineBridge` request while holding prefix coordination.
    fn with_bridge<R, Fut>(
        &self,
        work: impl FnOnce(WineBridgeClient) -> Fut + Send + 'static,
    ) -> Operation<R>
    where
        R: Send + 'static,
        Fut: Future<Output = Result<R>> + Send + 'static,
    {
        let prefix = self.clone();
        Operation::new(move |job| async move {
            let _control = job.cancellable(prefix.0.control.lock()).await?;
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
        let wineprefix = self.0.root.join("prefix");
        let env = state.env();
        self.stop_locked().await?;
        let runner = state.runner.runner(state.umu.as_ref())?;
        let command = state.wrappers.apply(WineBridgeClient::command(
            runner.as_ref(),
            &wineprefix,
            state.winebridge.path(),
            env.iter(),
        ));
        let result = async {
            self.0.storage.mount(state, &self.0.root).await?;
            job.check_cancellation()?;
            let bridge = WineBridgeClient::connect_or_spawn(&wineprefix, command).await?;
            state.write_registry(&bridge).await?;
            job.check_cancellation().map(|()| bridge)
        }
        .await;
        if result.is_err() {
            stop(runner.as_ref(), &wineprefix).await?;
            self.0.storage.release(&self.0.root).await?;
        } else {
            self.0.bridge_started.send_replace(());
            tracing::info!("WineBridge started");
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
    WineBridgeClient::shutdown_existing(wineprefix)
        .await
        .log_debug();
    for argument in ["-k", "-w"] {
        runner.wineserver(wineprefix, argument).await?;
    }
    WineBridgeClient::clear_discovery(wineprefix).await?;
    Ok(())
}
