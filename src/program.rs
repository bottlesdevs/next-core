//! Standalone launch definitions backed by dedicated Virgo environments.
//!
//! Unlike a program registered inside a [`crate::Bottle`], a [`Program`] owns
//! its environment configuration, private storage, and snapshot history.

use crate::{
    Addon, Component, Edit, LibraryEntry, LibraryProvider, Manager, Operation, PrefixBackend,
    ProgramSpec, Snapshot, SnapshotSummary, State,
    environment::{BackendSource, Environment},
    error::{Error, Result},
    manager::Managed,
    proto::{DllOverride, DllOverrideMode, Process},
};
use futures_core::Stream;
use std::sync::Arc;
use uuid::Uuid;

/// An immutable snapshot of a standalone program and its environment configuration.
pub type ProgramState = State<ProgramSpec>;

impl State<ProgramSpec> {
    /// Returns the display name from the launch definition.
    pub fn name(&self) -> &str {
        self.data.name()
    }
    /// Returns the persisted launch definition.
    pub fn launch(&self) -> &ProgramSpec {
        &self.data
    }
}

impl BackendSource for ProgramSpec {
    fn backend(&self) -> PrefixBackend {
        PrefixBackend::Virgo
    }
}

/// A live handle to a standalone Virgo program environment.
///
/// Clones share state and lifecycle coordination. Dropping the handle does not
/// stop Wine; use [`Program::stop`] for explicit shutdown. A handle retained
/// after [`Manager::delete`] reports
/// [`EnvironmentError::Deleted`](crate::EnvironmentError::Deleted).
#[derive(Clone)]
pub struct Program(pub(crate) Arc<Environment<ProgramSpec>>);

impl Managed for Program {
    type Data = ProgramSpec;

    fn from_environment(environment: Arc<Environment<ProgramSpec>>) -> Self {
        Self(environment)
    }

    fn environment(&self) -> &Environment<ProgramSpec> {
        &self.0
    }
}

impl Program {
    /// Returns the program's persistent identifier.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::Deleted`](crate::EnvironmentError::Deleted)
    /// after the program is deleted.
    pub fn id(&self) -> Result<Uuid> {
        Ok(self.state()?.id())
    }
    /// Returns the currently published state snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::Deleted`](crate::EnvironmentError::Deleted)
    /// after the program is deleted.
    pub fn state(&self) -> Result<Arc<ProgramState>> {
        self.0.state()
    }
    /// Streams the current state and later published replacements.
    ///
    /// The first item is the state current at subscription time. Slow consumers
    /// may miss intermediate publications, and deletion ends the stream.
    pub fn watch(&self) -> impl Stream<Item = Arc<ProgramState>> + Send + 'static + use<> {
        self.0.watch()
    }

    /// Creates an operation that edits launch settings and software selections.
    ///
    /// The callback changes a private draft. The final state is validated,
    /// applied, persisted, and then published once. Software changes require the
    /// program to be stopped; launch metadata, environment variables, and
    /// wrappers may be edited while it is running. An unchanged draft is not saved.
    /// Any shutdown begun for the edit finishes before cancellation is returned.
    /// Once application succeeds, persistence and publication are not cancelled.
    /// Failures after checkpoint capture recover the previous files before
    /// returning; recovery failure reports the program root requiring repair.
    ///
    /// # Errors
    ///
    /// Awaiting the operation returns callback, validation, cancellation,
    /// persistence, software application, or Virgo recovery errors.
    /// [`EnvironmentError::MustBeStopped`](crate::EnvironmentError::MustBeStopped)
    /// is returned when software selections change while Wine is running.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Program;
    /// # fn prepare(program: &Program) {
    /// let rename = program.edit(|edit| {
    ///     edit.rename("Updated");
    ///     Ok(())
    /// });
    /// # let _ = rename;
    /// # }
    /// ```
    pub fn edit<R: Send + 'static>(
        &self,
        callback: impl FnOnce(&mut Edit<'_, ProgramSpec>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        self.0.edit(callback)
    }
    /// Creates an operation that mounts storage and launches the program.
    ///
    /// The latest published launch definition is resolved after operation
    /// coordination is acquired. The program UUID is used as the `WineBridge`
    /// process-group identifier. Success returns the new process ID; it does not
    /// wait for the process to exit. Cancellation does not interrupt an in-flight
    /// launch request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation may fail during storage mounting, runner or
    /// `WineBridge` startup, cancellation, or process launch.
    pub fn launch(&self) -> Operation<u32> {
        self.0.launch(|state| Ok((state.id, state.data.clone())))
    }
    /// Terminates the program's process group without starting a stopped runtime.
    /// A stopped runtime is a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if state or `WineBridge` discovery fails, or `WineBridge`
    /// cannot terminate the process group.
    pub async fn kill(&self) -> Result<()> {
        self.0.kill(|state| Ok(state.id)).await
    }
    /// Stops Wine and releases the program's mounted Virgo storage.
    ///
    /// # Errors
    ///
    /// Returns an error if runner resolution, Wine shutdown, unmounting, or
    /// discovery cleanup fails.
    pub async fn stop(&self) -> Result<()> {
        self.0.stop().await
    }
    /// Lists processes currently reported by `WineBridge`.
    ///
    /// A stopped program returns an empty vector.
    ///
    /// # Errors
    ///
    /// Returns an error if state or discovery fails, or `WineBridge` cannot list
    /// its processes.
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.0.processes().await
    }
    /// Creates an operation that lists configured DLL overrides.
    ///
    /// A stopped runtime is started and left running. Cancellation does not
    /// interrupt an in-flight `WineBridge` request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during storage, runtime, cancellation,
    /// or `WineBridge` work.
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.0.dll_overrides()
    }
    /// Creates an operation that sets a Wine DLL override.
    ///
    /// A stopped runtime is started and left running. Cancellation does not
    /// interrupt an in-flight `WineBridge` request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during storage, runtime, cancellation,
    /// or `WineBridge` work.
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        self.0.set_dll_override(dll.into(), mode)
    }
    /// Creates an operation that removes a Wine DLL override.
    ///
    /// A stopped runtime is started and left running. A missing override is a
    /// successful no-op. Cancellation does not interrupt an in-flight
    /// `WineBridge` request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during storage, runtime, cancellation,
    /// or `WineBridge` work.
    pub fn unset_dll_override(&self, dll: impl Into<String>) -> Operation<()> {
        self.0.unset_dll_override(dll.into())
    }
    /// Creates an operation that snapshots persistent files and `state.toml`.
    ///
    /// Shared layers and external files are excluded. Explicit snapshots create
    /// a revision even if files have not changed. The program is stopped before
    /// capture and is not restarted afterward. Shutdown and capture are not
    /// interrupted by cancellation; [`Operation::cancel`] waits for the current
    /// step to finish.
    ///
    /// # Errors
    ///
    /// Awaiting the operation returns an invalid-input I/O error when `message`
    /// is the reserved internal checkpoint message. It can also fail during
    /// cancellation, Wine shutdown, repository initialization, or commit.
    pub fn create_snapshot(&self, message: impl Into<String>) -> Operation<Snapshot> {
        self.0.create_snapshot(message.into())
    }
    /// Lists user snapshots from newest to oldest.
    ///
    /// Internal recovery checkpoints are excluded. A program with no history
    /// returns an empty vector without contacting FVS.
    ///
    /// # Errors
    ///
    /// Returns an error if the program was deleted, repository metadata cannot
    /// be inspected, or FVS cannot list commits.
    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        self.0.snapshots().await
    }
    /// Creates an operation that restores and publishes a historical state.
    ///
    /// The restored UUID must match the program and its configuration must
    /// validate. Failed restoration recovers the pre-restore checkpoint before
    /// returning. Shutdown and checkpoint capture finish before cancellation is
    /// observed. Once restoration begins, cancellation does not interrupt
    /// restoration or recovery. Working files change without moving the FVS
    /// repository's current commit. Rollback first attempts to stop the program
    /// and never restarts it.
    ///
    /// On success, the operation returns the full state ID resolved from
    /// `revision` and publishes the launch and environment state stored there.
    ///
    /// # Errors
    ///
    /// Awaiting the operation fails on cancellation before restoration, Wine
    /// shutdown or checkpoint failure, an unknown revision, mismatched or
    /// invalid restored state, or failed restoration or recovery.
    pub fn rollback(&self, revision: &str) -> Operation<String> {
        self.0.rollback(revision)
    }
}
impl Edit<'_, ProgramSpec> {
    /// Replaces the standalone program's display name.
    pub fn rename(&mut self, name: impl Into<String>) {
        self.draft.data.rename(name);
    }

    /// Returns the mutable launch definition in the edit draft.
    pub fn launch(&mut self) -> &mut ProgramSpec {
        &mut self.draft.data
    }
}

#[async_trait::async_trait]
impl LibraryProvider for Manager<Program> {
    fn id(&self) -> &str {
        "programs"
    }

    async fn list_entries(&self) -> Result<Vec<LibraryEntry>> {
        Ok(self
            .list()
            .into_iter()
            .filter_map(|program| program.state().ok())
            .map(|state| LibraryEntry {
                id: state.id().to_string(),
                title: state.name().to_owned(),
            })
            .collect())
    }

    fn launch(&self, entry_id: &str) -> Result<Operation<()>> {
        let id = Uuid::parse_str(entry_id).map_err(|error| Error::LibraryProvider {
            provider: self.id().to_owned(),
            message: error.to_string(),
        })?;
        Ok(self.open(id)?.launch().map(|_| ()))
    }
}
impl Manager<Program> {
    /// Creates an operation that initializes a standalone Virgo program.
    ///
    /// It builds missing shared layers, prepares private registry state, and
    /// persists the launch definition. It does not download the application.
    /// The program is added to this manager only after initialization and
    /// `state.toml` persistence succeed. Cancellation or persistence failure
    /// after backend initialization attempts best-effort removal; earlier
    /// backend failures, dropping a started operation, or cleanup failure can
    /// leave an unregistered directory that a later core startup may discover.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during requirement validation, shared
    /// layer creation, private storage preparation, cancellation, or persistence.
    pub fn create(
        &self,
        launch: ProgramSpec,
        runner: Addon<Component>,
        winebridge: Addon<Component>,
        umu: Option<Addon<Component>>,
    ) -> Operation<Program> {
        self.create_environment(launch, runner, winebridge, umu)
    }
}
