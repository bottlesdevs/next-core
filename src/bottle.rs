//! Persisted Wine-prefix environments and their registered programs.
//!
//! A [`Bottle`] is a live handle owned by a [`Manager<Bottle>`](crate::Manager).
//! Reading produces immutable [`BottleState`] snapshots; editing produces a
//! private draft that is validated and saved before it replaces the published
//! state.

use crate::{
    Addon, Component, Edit, LibraryEntry, LibraryProvider, Manager, Operation, PrefixBackend,
    ProgramSpec, State,
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
};
#[cfg(feature = "fvs")]
use crate::{Snapshot, SnapshotSummary};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

/// Bottle-owned data stored alongside the shared [`EnvironmentConfig`](crate::EnvironmentConfig).
///
/// Callers read this data through [`BottleState`] and modify it through
/// [`Bottle::edit`] rather than constructing the type directly.
///
/// # Examples
///
/// ```
/// # use bottles_core::BottleState;
/// # fn print(state: &BottleState) {
/// println!("{} has {} programs", state.name(), state.programs().count());
/// # }
/// ```
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BottleData {
    pub(crate) name: String,
    pub(crate) backend: PrefixBackend,
    #[serde(default)]
    pub(crate) programs: HashMap<Uuid, ProgramSpec>,
}

/// An immutable snapshot of a bottle's published data and environment configuration.
///
/// # Examples
///
/// ```
/// # use bottles_core::BottleState;
/// # fn inspect(state: &BottleState) {
/// println!("{} uses {:?}", state.name(), state.backend());
/// # }
/// ```
pub type BottleState = State<BottleData>;

impl crate::environment::BackendSource for BottleData {
    fn backend(&self) -> PrefixBackend {
        self.backend
    }
}

impl State<BottleData> {
    /// Returns the bottle's display name.
    ///
    /// Names are not identities and need not be unique.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::BottleState;
    /// # fn name(state: &BottleState) -> &str {
    /// state.name()
    /// # }
    /// ```
    pub fn name(&self) -> &str {
        &self.data.name
    }

    /// Iterates over registered program identifiers and launch definitions.
    ///
    /// Iteration order is unspecified.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::BottleState;
    /// # fn inspect(state: &BottleState) {
    /// for (id, program) in state.programs() {
    ///     println!("{id}: {}", program.name());
    /// }
    /// # }
    /// ```
    pub fn programs(&self) -> impl Iterator<Item = (Uuid, &ProgramSpec)> {
        self.data.programs.iter().map(|(id, launch)| (*id, launch))
    }

    /// Returns the prefix backend chosen when the bottle was created.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{BottleState, PrefixBackend};
    /// # fn is_standard(state: &BottleState) -> bool {
    /// state.backend() == PrefixBackend::Standard
    /// # }
    /// ```
    pub fn backend(&self) -> PrefixBackend {
        self.data.backend
    }

    /// Returns the program registered under `id`, if present.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{BottleState, ProgramSpec};
    /// # use uuid::Uuid;
    /// # fn find(state: &BottleState, id: Uuid) -> Option<&ProgramSpec> {
    /// state.program(id)
    /// # }
    /// ```
    pub fn program(&self, id: Uuid) -> Option<&ProgramSpec> {
        self.data.programs.get(&id)
    }
}

/// A live handle to one persisted Wine-prefix environment.
///
/// Clones share state and operation coordination. Dropping the last handle does
/// not stop Wine; call [`Bottle::stop`] when shutdown is required. A handle kept
/// after [`Manager::delete`](crate::Manager::delete) reports
/// [`EnvironmentError::Deleted`](crate::EnvironmentError::Deleted).
///
/// # Examples
///
/// ```
/// # use bottles_core::Bottle;
/// # fn inspect(bottle: &Bottle) -> Result<(), bottles_core::error::Error> {
/// let state = bottle.state()?;
/// println!("{}", state.name());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Bottle(pub(crate) Arc<crate::environment::Environment<BottleData>>);

impl crate::manager::Managed for Bottle {
    type Data = BottleData;

    fn from_environment(environment: Arc<crate::environment::Environment<BottleData>>) -> Self {
        Self(environment)
    }

    fn environment(&self) -> &crate::environment::Environment<BottleData> {
        &self.0
    }
}

impl Bottle {
    /// Returns the bottle's persistent identifier.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::Deleted`](crate::EnvironmentError::Deleted)
    /// if deletion has already been published.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottle;
    /// # fn id(bottle: &Bottle) -> Result<uuid::Uuid, bottles_core::error::Error> {
    /// bottle.id()
    /// # }
    /// ```
    pub fn id(&self) -> Result<Uuid> {
        Ok(self.state()?.id())
    }
    /// Returns the currently published immutable state snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::Deleted`](crate::EnvironmentError::Deleted)
    /// after the bottle is deleted.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Bottle, BottleState};
    /// # use std::sync::Arc;
    /// # fn state(bottle: &Bottle) -> Result<Arc<BottleState>, bottles_core::error::Error> {
    /// bottle.state()
    /// # }
    /// ```
    pub fn state(&self) -> Result<Arc<BottleState>> {
        self.0.state()
    }

    /// Streams the current state and later published replacements.
    ///
    /// The first item is the state current at subscription time. Slow consumers
    /// may miss intermediate states, and deletion ends the stream.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottle;
    /// # use futures_lite::StreamExt;
    /// # async fn observe(bottle: &Bottle) {
    /// let mut states = bottle.watch();
    /// if let Some(state) = states.next().await {
    ///     println!("{}", state.name());
    /// }
    /// # }
    /// ```
    pub fn watch(&self) -> impl Stream<Item = Arc<BottleState>> + Send + 'static + use<> {
        self.0.watch()
    }
}

impl Bottle {
    /// Creates an operation that lists configured DLL overrides.
    ///
    /// The operation starts or reconnects to WineBridge if necessary.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during environment access, runner or
    /// WineBridge startup, cancellation, or the WineBridge request.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Bottle, Operation};
    /// # use bottles_core::DllOverride;
    /// # fn prepare(bottle: &Bottle) -> Operation<Vec<DllOverride>> {
    /// bottle.dll_overrides()
    /// # }
    /// ```
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.0.dll_overrides()
    }
    /// Creates an operation that sets one Wine DLL override.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during environment access, runner or
    /// WineBridge startup, cancellation, or the WineBridge request.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Bottle, Operation};
    /// # use bottles_core::DllOverrideMode;
    /// # fn prepare(bottle: &Bottle, mode: DllOverrideMode) -> Operation<()> {
    /// bottle.set_dll_override("d3d11", mode)
    /// # }
    /// ```
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        self.0.set_dll_override(dll.into(), mode)
    }
    /// Creates an operation that removes one Wine DLL override.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during environment access, runner or
    /// WineBridge startup, cancellation, or the WineBridge request.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Bottle, Operation};
    /// # fn prepare(bottle: &Bottle) -> Operation<()> {
    /// bottle.unset_dll_override("d3d11")
    /// # }
    /// ```
    pub fn unset_dll_override(&self, dll: impl Into<String>) -> Operation<()> {
        self.0.unset_dll_override(dll.into())
    }

    /// Creates an operation that launches a registered program.
    ///
    /// The registration is resolved from the latest state after operation
    /// coordination is acquired.
    ///
    /// # Errors
    ///
    /// Awaiting the operation returns [`BottleError::ProgramNotFound`] if `id`
    /// is not registered, and may return environment, runner, cancellation, or
    /// WineBridge errors while starting the process.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Bottle, Operation};
    /// # use uuid::Uuid;
    /// # fn prepare(bottle: &Bottle, id: Uuid) -> Operation<u32> {
    /// bottle.launch_program(id)
    /// # }
    /// ```
    pub fn launch_program(&self, id: Uuid) -> Operation<u32> {
        self.0.launch(move |state| {
            let launch = state
                .program(id)
                .cloned()
                .ok_or(BottleError::ProgramNotFound(id))?;
            Ok((id, launch))
        })
    }
    /// Creates an operation that launches an unregistered program definition.
    ///
    /// `id` becomes the WineBridge process-group identifier but is not persisted
    /// in the bottle.
    ///
    /// # Errors
    ///
    /// Awaiting the operation may return environment, runner, cancellation, or
    /// WineBridge errors while starting the process.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Bottle, Operation, ProgramSpec};
    /// # use uuid::Uuid;
    /// # fn prepare(bottle: &Bottle) -> Operation<u32> {
    /// let program = ProgramSpec::new("Setup", "setup.exe");
    /// bottle.launch(Uuid::new_v4(), program)
    /// # }
    /// ```
    pub fn launch(&self, id: Uuid, launch: ProgramSpec) -> Operation<u32> {
        self.0.launch(move |_| Ok((id, launch)))
    }
    /// Lists processes currently reported by WineBridge.
    ///
    /// A stopped bottle returns an empty vector and is not started.
    ///
    /// # Errors
    ///
    /// Returns an error if the bottle was deleted, discovery fails, or the
    /// connected WineBridge cannot list its processes.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottle;
    /// # async fn example(bottle: &Bottle) -> Result<(), bottles_core::error::Error> {
    /// let processes = bottle.processes().await?;
    /// # let _ = processes;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.0.processes().await
    }
    /// Terminates a WineBridge process group without starting a stopped bottle.
    ///
    /// The identifier need not belong to a saved program. A stopped runtime is
    /// a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the bottle was deleted, discovery fails, or
    /// WineBridge cannot terminate the process group.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottle;
    /// # use uuid::Uuid;
    /// # async fn example(bottle: &Bottle, id: Uuid) -> Result<(), bottles_core::error::Error> {
    /// bottle.kill_program(id).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn kill_program(&self, id: Uuid) -> Result<()> {
        self.0.kill(|_| Ok(id)).await
    }
    /// Stops Wine and releases storage mounted for the bottle.
    ///
    /// Calling this for an already stopped bottle is safe.
    ///
    /// # Errors
    ///
    /// Returns an error if state or runner resolution, Wine shutdown, storage
    /// release, or discovery cleanup fails.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottle;
    /// # async fn example(bottle: &Bottle) -> Result<(), bottles_core::error::Error> {
    /// bottle.stop().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn stop(&self) -> Result<()> {
        self.0.stop().await
    }
}

impl Bottle {
    /// Creates an operation that edits a private draft of the latest state.
    ///
    /// The final draft is validated, applied, saved, and then published once.
    /// Software selection changes require a stopped bottle; metadata and startup
    /// settings may change while Wine is running. An unchanged draft is not saved.
    ///
    /// # Errors
    ///
    /// Awaiting the operation returns errors from the callback, validation,
    /// cancellation, persistence, software application, or storage recovery.
    /// [`EnvironmentError::MustBeStopped`](crate::EnvironmentError::MustBeStopped)
    /// is returned when software changes are requested while Wine is running.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottle;
    /// # fn prepare(bottle: &Bottle) {
    /// let rename = bottle.edit(|edit| {
    ///     edit.rename("Development");
    ///     Ok(())
    /// });
    /// # let _ = rename;
    /// # }
    /// ```
    pub fn edit<R: Send + 'static>(
        &self,
        callback: impl FnOnce(&mut Edit<'_, BottleData>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        self.0.edit(callback)
    }
}

impl Edit<'_, BottleData> {
    /// Replaces the bottle's display name.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{BottleData, Edit};
    /// # fn rename(edit: &mut Edit<'_, BottleData>) {
    /// edit.rename("Games");
    /// # }
    /// ```
    pub fn rename(&mut self, name: impl Into<String>) {
        self.draft.data.name = name.into();
    }

    /// Registers a launch definition and returns its generated identifier.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{BottleData, Edit, ProgramSpec};
    /// # fn add(edit: &mut Edit<'_, BottleData>) {
    /// let id = edit.add_program(ProgramSpec::new("Tool", "tool.exe"));
    /// assert!(edit.program(id).is_some());
    /// # }
    /// ```
    pub fn add_program(&mut self, launch: ProgramSpec) -> Uuid {
        let id = Uuid::new_v4();
        self.draft.data.programs.insert(id, launch);
        id
    }

    /// Removes and returns the program registered under `id`.
    ///
    /// Returns `None` when no such registration exists.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{BottleData, Edit, ProgramSpec};
    /// # fn remove(edit: &mut Edit<'_, BottleData>) {
    /// let id = edit.add_program(ProgramSpec::new("Tool", "tool.exe"));
    /// assert!(edit.remove_program(id).is_some());
    /// # }
    /// ```
    pub fn remove_program(&mut self, id: Uuid) -> Option<ProgramSpec> {
        self.draft.data.programs.remove(&id)
    }

    /// Returns a mutable launch definition without changing its registration ID.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{BottleData, Edit};
    /// # use uuid::Uuid;
    /// # fn rename_program(edit: &mut Edit<'_, BottleData>, id: Uuid) {
    /// if let Some(program) = edit.program(id) {
    ///     program.rename("Updated name");
    /// }
    /// # }
    /// ```
    pub fn program(&mut self, id: Uuid) -> Option<&mut ProgramSpec> {
        self.draft.data.programs.get_mut(&id)
    }
}

#[cfg(feature = "fvs")]
impl Bottle {
    /// Creates an operation that captures the bottle's managed state.
    /// Shared artifacts and external files are not copied or rebuilt on restoration.
    /// Explicit snapshots create a revision even without changes.
    /// The internal checkpoint message is reserved. Once capture starts it finishes
    /// under coordination, including when explicit cancellation is requested.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail while stopping Wine, reading managed
    /// files, or committing the FVS revision.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Bottle, Operation, Snapshot};
    /// # fn prepare(bottle: &Bottle) -> Operation<Snapshot> {
    /// bottle.create_snapshot("Before update")
    /// # }
    /// ```
    pub fn create_snapshot(&self, message: impl Into<String>) -> Operation<Snapshot> {
        self.0.create_snapshot(message.into())
    }

    /// Lists user snapshots from newest to oldest.
    ///
    /// Internal checkpoints are excluded. A bottle with no history returns an
    /// empty vector without contacting FVS.
    ///
    /// # Errors
    ///
    /// Returns an error if history metadata cannot be read or FVS cannot list
    /// revisions.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottle;
    /// # async fn example(bottle: &Bottle) -> Result<(), bottles_core::error::Error> {
    /// let history = bottle.snapshots().await?;
    /// # let _ = history;
    /// # Ok(())
    /// # }
    /// ```
    /// A bottle without history returns an empty list without contacting FVS.
    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        self.0.snapshots().await
    }

    /// Creates an operation that restores a snapshot into the managed root.
    /// UUID, backend and configuration format must match. A failed restore or
    /// invalid state recovers the previous files before returning; failed recovery
    /// reports the root requiring repair. Cancellation does not interrupt an active
    /// restore or recovery. Working files change without moving FVS's current commit.
    ///
    /// # Errors
    ///
    /// Awaiting the operation fails if the revision is invalid, restored state
    /// does not belong to this bottle, or restoration and recovery cannot finish.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Bottle, Operation};
    /// # fn prepare(bottle: &Bottle, revision: &str) -> Operation<String> {
    /// bottle.rollback(revision)
    /// # }
    /// ```
    pub fn rollback(&self, revision: &str) -> Operation<String> {
        self.0.rollback(revision)
    }
}

/// Bottle-specific request failures.
///
/// # Examples
///
/// ```
/// use bottles_core::BottleError;
/// use uuid::Uuid;
///
/// let error = BottleError::ProgramNotFound(Uuid::nil());
/// assert!(error.to_string().contains("was not found"));
/// ```
#[derive(Debug, thiserror::Error)]
pub enum BottleError {
    /// No program is registered with the requested UUID.
    #[error("program {0} was not found")]
    ProgramNotFound(Uuid),
}

#[async_trait::async_trait]
impl LibraryProvider for Manager<Bottle> {
    fn id(&self) -> &str {
        "bottles"
    }

    async fn list_entries(&self) -> Result<Vec<LibraryEntry>> {
        let mut entries = Vec::new();
        for state in self
            .list()
            .into_iter()
            .filter_map(|bottle| bottle.state().ok())
        {
            entries.extend(state.programs().map(|(id, program)| LibraryEntry {
                id: format!("{}/{id}", state.id()),
                title: program.name().to_owned(),
            }));
        }
        Ok(entries)
    }

    fn launch(&self, entry_id: &str) -> Result<Operation<()>> {
        let (bottle_id, program_id) =
            entry_id
                .split_once('/')
                .ok_or_else(|| Error::LibraryProvider {
                    provider: self.id().to_owned(),
                    message: "expected bottle UUID/program UUID".into(),
                })?;
        let parse_id = |id| {
            Uuid::parse_str(id).map_err(|error| Error::LibraryProvider {
                provider: self.id().to_owned(),
                message: error.to_string(),
            })
        };
        Ok(self
            .open(parse_id(bottle_id)?)?
            .launch_program(parse_id(program_id)?)
            .map(|_| ()))
    }
}

impl Manager<Bottle> {
    /// Creates an operation that initializes and persists a bottle.
    ///
    /// A new UUID is assigned when the operation starts;
    /// display names are stored verbatim, may be empty, and need not be unique.
    /// Callers supply the runner, WineBridge, and optional UMU records. Their slots
    /// and coexistence requirements are checked before creating files; missing
    /// requirements fail without selecting or downloading other owner components.
    /// Standard creation initializes Wine without FVS. Virgo
    /// creation builds missing shared layers before creating private storage and
    /// composing its registry and saving selections. Startup mounts prepared storage.
    /// Failures and cancellation observed while the operation remains polled remove the
    /// partially-created bottle directory on a best-effort basis. Dropping a
    /// started operation or a cleanup failure can leave a directory that a
    /// later library startup discovers.
    ///
    /// # Arguments
    ///
    /// * `name` - Display name stored verbatim; names need not be unique.
    /// * `backend` - Prefix storage strategy, fixed for the bottle's lifetime.
    /// * `runner` - Wine or Proton component used to create and run the prefix.
    /// * `winebridge` - WineBridge component used for process and prefix control.
    /// * `umu` - Optional UMU component required by compatible runners.
    ///
    /// # Errors
    ///
    /// Returns [`crate::EnvironmentError::RequiresAddon`] for missing coexistence
    /// requirements before creating any files. Other service, I/O, and prefix
    /// creation failures are returned directly.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Addon, Bottle, Component, Manager, Operation, PrefixBackend};
    /// # fn prepare(
    /// #     manager: &Manager<Bottle>,
    /// #     runner: Addon<Component>,
    /// #     winebridge: Addon<Component>,
    /// # ) -> Operation<Bottle> {
    /// manager.create("Games", PrefixBackend::Standard, runner, winebridge, None)
    /// # }
    /// ```
    pub fn create(
        &self,
        name: impl Into<String>,
        backend: PrefixBackend,
        runner: Addon<Component>,
        winebridge: Addon<Component>,
        umu: Option<Addon<Component>>,
    ) -> Operation<Bottle> {
        self.create_environment(
            BottleData {
                name: name.into(),
                backend,
                programs: HashMap::new(),
            },
            runner,
            winebridge,
            umu,
        )
    }
}
