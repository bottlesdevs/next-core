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
///
/// # Examples
///
/// ```
/// # use bottles_core::ProgramState;
/// # fn inspect(state: &ProgramState) {
/// println!("{}: {}", state.name(), state.launch().executable());
/// # }
/// ```
pub type ProgramState = State<ProgramSpec>;

impl State<ProgramSpec> {
    /// Returns the display name from the launch definition.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::ProgramState;
    /// # fn name(state: &ProgramState) -> &str {
    /// state.name()
    /// # }
    /// ```
    pub fn name(&self) -> &str {
        self.data.name()
    }
    /// Returns the persisted launch definition.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{ProgramSpec, ProgramState};
    /// # fn launch(state: &ProgramState) -> &ProgramSpec {
    /// state.launch()
    /// # }
    /// ```
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
/// stop Wine; use [`Program::stop`] for explicit shutdown.
///
/// # Examples
///
/// ```
/// # use bottles_core::Program;
/// # fn inspect(program: &Program) -> Result<(), bottles_core::error::Error> {
/// println!("{}", program.state()?.name());
/// # Ok(())
/// # }
/// ```
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
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Program;
    /// # fn id(program: &Program) -> Result<uuid::Uuid, bottles_core::error::Error> {
    /// program.id()
    /// # }
    /// ```
    pub fn id(&self) -> Result<Uuid> {
        Ok(self.state()?.id())
    }
    /// Returns the currently published state snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::Deleted`](crate::EnvironmentError::Deleted)
    /// after the program is deleted.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use bottles_core::{Program, ProgramState};
    /// # fn state(program: &Program) -> Result<Arc<ProgramState>, bottles_core::error::Error> {
    /// program.state()
    /// # }
    /// ```
    pub fn state(&self) -> Result<Arc<ProgramState>> {
        self.0.state()
    }
    /// Streams the current state and later published replacements.
    ///
    /// Slow consumers may miss intermediate publications. Deletion ends the stream.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Program;
    /// # use futures_lite::StreamExt;
    /// # async fn observe(program: &Program) {
    /// let mut states = program.watch();
    /// if let Some(state) = states.next().await {
    ///     println!("{}", state.name());
    /// }
    /// # }
    /// ```
    pub fn watch(&self) -> impl Stream<Item = Arc<ProgramState>> + Send + 'static + use<> {
        self.0.watch()
    }

    /// Creates an operation that edits launch settings and software selections.
    ///
    /// The callback changes a private draft. The final state is validated,
    /// applied, persisted, and then published once. Software changes require the
    /// program to be stopped.
    ///
    /// # Errors
    ///
    /// Awaiting the operation returns callback, validation, cancellation,
    /// persistence, software application, or Virgo recovery errors.
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
    /// The program UUID is used as the WineBridge process-group identifier.
    ///
    /// # Errors
    ///
    /// Awaiting the operation may fail during storage mounting, runner or
    /// WineBridge startup, cancellation, or process launch.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Operation, Program};
    /// # fn prepare(program: &Program) -> Operation<u32> {
    /// program.launch()
    /// # }
    /// ```
    pub fn launch(&self) -> Operation<u32> {
        self.0.launch(|state| Ok((state.id, state.data.clone())))
    }
    /// Terminates the program's process group without starting a stopped runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if state or WineBridge discovery fails, or WineBridge
    /// cannot terminate the process group.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Program;
    /// # async fn example(program: &Program) -> Result<(), bottles_core::error::Error> {
    /// program.kill().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn kill(&self) -> Result<()> {
        self.0.kill(|state| Ok(state.id)).await
    }
    /// Stops Wine and releases the program's mounted Virgo storage.
    ///
    /// # Errors
    ///
    /// Returns an error if runner resolution, Wine shutdown, unmounting, or
    /// discovery cleanup fails.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Program;
    /// # async fn example(program: &Program) -> Result<(), bottles_core::error::Error> {
    /// program.stop().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn stop(&self) -> Result<()> {
        self.0.stop().await
    }
    /// Lists processes currently reported by WineBridge.
    ///
    /// A stopped program returns an empty vector.
    ///
    /// # Errors
    ///
    /// Returns an error if state or discovery fails, or WineBridge cannot list
    /// its processes.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Program;
    /// # async fn example(program: &Program) -> Result<(), bottles_core::error::Error> {
    /// let processes = program.processes().await?;
    /// # let _ = processes;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.0.processes().await
    }
    /// Creates an operation that lists configured DLL overrides.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during storage, runtime, cancellation,
    /// or WineBridge work.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Operation, Program};
    /// # use bottles_core::DllOverride;
    /// # fn prepare(program: &Program) -> Operation<Vec<DllOverride>> {
    /// program.dll_overrides()
    /// # }
    /// ```
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.0.dll_overrides()
    }
    /// Creates an operation that sets a Wine DLL override.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during storage, runtime, cancellation,
    /// or WineBridge work.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Operation, Program};
    /// # use bottles_core::DllOverrideMode;
    /// # fn prepare(program: &Program, mode: DllOverrideMode) -> Operation<()> {
    /// program.set_dll_override("d3d11", mode)
    /// # }
    /// ```
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        self.0.set_dll_override(dll.into(), mode)
    }
    /// Creates an operation that removes a Wine DLL override.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during storage, runtime, cancellation,
    /// or WineBridge work.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Operation, Program};
    /// # fn prepare(program: &Program) -> Operation<()> {
    /// program.unset_dll_override("d3d11")
    /// # }
    /// ```
    pub fn unset_dll_override(&self, dll: impl Into<String>) -> Operation<()> {
        self.0.unset_dll_override(dll.into())
    }
    /// Creates an operation that snapshots persistent files and `state.toml`.
    ///
    /// Shared layers and external files are excluded.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail while stopping Wine, reading managed
    /// files, or committing the FVS revision.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Operation, Program, Snapshot};
    /// # fn prepare(program: &Program) -> Operation<Snapshot> {
    /// program.create_snapshot("Before update")
    /// # }
    /// ```
    pub fn create_snapshot(&self, message: impl Into<String>) -> Operation<Snapshot> {
        self.0.create_snapshot(message.into())
    }
    /// Lists user snapshots from newest to oldest.
    ///
    /// # Errors
    ///
    /// Returns an error if history metadata or FVS revisions cannot be read.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Program;
    /// # async fn example(program: &Program) -> Result<(), bottles_core::error::Error> {
    /// let history = program.snapshots().await?;
    /// # let _ = history;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        self.0.snapshots().await
    }
    /// Creates an operation that restores and publishes a historical state.
    ///
    /// Failed restoration recovers the pre-restore checkpoint before returning.
    ///
    /// # Errors
    ///
    /// Awaiting the operation fails if the revision is invalid, restored state
    /// does not belong to this program, or restoration and recovery cannot finish.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Operation, Program};
    /// # fn prepare(program: &Program, revision: &str) -> Operation<String> {
    /// program.rollback(revision)
    /// # }
    /// ```
    pub fn rollback(&self, revision: &str) -> Operation<String> {
        self.0.rollback(revision)
    }
}
impl Edit<'_, ProgramSpec> {
    /// Replaces the standalone program's display name.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Edit, ProgramSpec};
    /// # fn rename(edit: &mut Edit<'_, ProgramSpec>) {
    /// edit.rename("Updated");
    /// # }
    /// ```
    pub fn rename(&mut self, name: impl Into<String>) {
        self.draft.data.rename(name);
    }

    /// Returns the mutable launch definition in the edit draft.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Edit, ProgramSpec};
    /// # fn configure(edit: &mut Edit<'_, ProgramSpec>) {
    /// edit.launch().rename("Updated");
    /// # }
    /// ```
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
    ///
    /// # Arguments
    ///
    /// * `launch` - Persisted launch definition and display name.
    /// * `runner` - Wine or Proton component used by the environment.
    /// * `winebridge` - WineBridge component used for process control.
    /// * `umu` - Optional UMU component required by compatible runners.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during requirement validation, shared
    /// layer creation, private storage preparation, cancellation, or persistence.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Addon, Component, Manager, Operation, Program, ProgramSpec};
    /// # fn prepare(
    /// #     manager: &Manager<Program>,
    /// #     runner: Addon<Component>,
    /// #     winebridge: Addon<Component>,
    /// # ) -> Operation<Program> {
    /// let launch = ProgramSpec::new("Tool", "tool.exe");
    /// manager.create(launch, runner, winebridge, None)
    /// # }
    /// ```
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
