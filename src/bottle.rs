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
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BottleData {
    pub(crate) name: String,
    pub(crate) backend: PrefixBackend,
    #[serde(default)]
    pub(crate) programs: HashMap<Uuid, ProgramSpec>,
}

/// An immutable snapshot of a bottle's published data and environment configuration.
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
    pub fn name(&self) -> &str {
        &self.data.name
    }

    /// Iterates over registered program identifiers and launch definitions.
    ///
    /// Iteration order is unspecified.
    pub fn programs(&self) -> impl Iterator<Item = (Uuid, &ProgramSpec)> {
        self.data.programs.iter().map(|(id, launch)| (*id, launch))
    }

    /// Returns the prefix backend chosen when the bottle was created.
    pub fn backend(&self) -> PrefixBackend {
        self.data.backend
    }

    /// Returns the program registered under `id`, if present.
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
    pub fn id(&self) -> Result<Uuid> {
        Ok(self.state()?.id())
    }
    /// Returns the currently published immutable state snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::Deleted`](crate::EnvironmentError::Deleted)
    /// after the bottle is deleted.
    pub fn state(&self) -> Result<Arc<BottleState>> {
        self.0.state()
    }

    /// Streams the current state and later published replacements.
    ///
    /// The first item is the state current at subscription time. Slow consumers
    /// may miss intermediate states, and deletion ends the stream.
    pub fn watch(&self) -> impl Stream<Item = Arc<BottleState>> + Send + 'static + use<> {
        self.0.watch()
    }
}

impl Bottle {
    /// Creates an operation that lists configured DLL overrides.
    ///
    /// The operation starts or reconnects to `WineBridge` if necessary.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during environment access, runner or
    /// `WineBridge` startup, cancellation, or the `WineBridge` request.
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.0.dll_overrides()
    }
    /// Creates an operation that sets one Wine DLL override.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during environment access, runner or
    /// `WineBridge` startup, cancellation, or the `WineBridge` request.
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        self.0.set_dll_override(dll.into(), mode)
    }
    /// Creates an operation that removes one Wine DLL override.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during environment access, runner or
    /// `WineBridge` startup, cancellation, or the `WineBridge` request.
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
    /// `WineBridge` errors while starting the process.
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
    /// `id` becomes the `WineBridge` process-group identifier but is not persisted
    /// in the bottle.
    ///
    /// # Errors
    ///
    /// Awaiting the operation may return environment, runner, cancellation, or
    /// `WineBridge` errors while starting the process.
    pub fn launch(&self, id: Uuid, launch: ProgramSpec) -> Operation<u32> {
        self.0.launch(move |_| Ok((id, launch)))
    }
    /// Lists processes currently reported by `WineBridge`.
    ///
    /// A stopped bottle returns an empty vector and is not started.
    ///
    /// # Errors
    ///
    /// Returns an error if the bottle was deleted, discovery fails, or the
    /// connected `WineBridge` cannot list its processes.
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.0.processes().await
    }
    /// Terminates a `WineBridge` process group without starting a stopped bottle.
    ///
    /// The identifier need not belong to a saved program. A stopped runtime is
    /// a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the bottle was deleted, discovery fails, or
    /// `WineBridge` cannot terminate the process group.
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
    pub fn rename(&mut self, name: impl Into<String>) {
        self.draft.data.name = name.into();
    }

    /// Registers a launch definition and returns its generated identifier.
    pub fn add_program(&mut self, launch: ProgramSpec) -> Uuid {
        let id = Uuid::new_v4();
        self.draft.data.programs.insert(id, launch);
        id
    }

    /// Removes and returns the program registered under `id`.
    ///
    /// Returns `None` when no such registration exists.
    pub fn remove_program(&mut self, id: Uuid) -> Option<ProgramSpec> {
        self.draft.data.programs.remove(&id)
    }

    /// Returns a mutable launch definition without changing its registration ID.
    pub fn program(&mut self, id: Uuid) -> Option<&mut ProgramSpec> {
        self.draft.data.programs.get_mut(&id)
    }
}

#[cfg(feature = "fvs")]
impl Bottle {
    /// Creates an operation that captures the bottle's managed state.
    ///
    /// Shared artifacts and external files are not copied or rebuilt on
    /// restoration. Explicit snapshots create a revision even without changes.
    /// Once capture starts, cooperative cancellation no longer interrupts it;
    /// [`Operation::cancel`] waits for the commit to finish.
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
    /// Internal checkpoints are excluded. A bottle with no history returns an
    /// empty vector without contacting FVS.
    ///
    /// # Errors
    ///
    /// Returns an error if history metadata cannot be read or FVS cannot list
    /// revisions.
    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        self.0.snapshots().await
    }

    /// Creates an operation that restores a snapshot into the managed root.
    ///
    /// The restored UUID and backend must match the bottle, and its environment
    /// configuration must validate. A failed restore or invalid state recovers
    /// the previous files before returning; failed recovery reports the root
    /// requiring repair. Cancellation is observed before restoration begins,
    /// but does not interrupt an active restore or recovery. Working files
    /// change without moving the FVS repository's current commit.
    ///
    /// On success, the operation returns the full state ID resolved from
    /// `revision` and publishes the state stored in that revision.
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

/// Bottle-specific request failures.
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
    /// Component slots and coexistence requirements are checked before creating
    /// files; missing requirements fail without selecting or downloading other
    /// owner components. Standard creation initializes Wine without FVS. Virgo
    /// creation builds missing shared layers before composing private storage.
    /// The bottle is added to this manager only after initialization and
    /// `state.toml` persistence succeed. Cancellation or persistence failure
    /// after backend initialization attempts best-effort removal; earlier
    /// backend failures, dropping a started operation, or cleanup failure can
    /// leave an unregistered directory that a later core startup may discover.
    ///
    /// # Errors
    ///
    /// Returns [`crate::EnvironmentError::RequiresAddon`] for missing coexistence
    /// requirements before creating any files. Awaiting the operation can also
    /// fail during cancellation, service startup, I/O, prefix initialization, or
    /// state persistence.
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
