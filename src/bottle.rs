//! Wine-prefix lifecycle and configuration.
//!
//! A [`crate::Manager`] owns the bottles known to one [`crate::Bottles`]
//! context. Its [`Bottle`] handles are live, cloneable references to shared
//! state; [`BottleState`] values returned by those handles are immutable
//! snapshots that do not change when the bottle is edited or deleted.
//! [`Bottle::edit`] applies a callback to the latest state and publishes it after
//! validation, software application, and persistence. [`crate::ProgramSpec`] defines
//! programs registered through that callback.
//!
//! Bottle directories and their `state.toml` files are library-managed.
//! Manager queries read an in-memory registry rather than rescanning or
//! reloading externally modified files. Component and dependency records are pinned in each
//! persisted state until a bottle operation explicitly replaces them.
//!
//! With the default `fvs` feature, bottles support caller-visible snapshots.
//! Standard history is created on demand; Virgo checkpoints software edits.
//! Long-running mutations return lazy
//! [`crate::Operation`] values and serialize with edits, stopping, snapshots,
//! and deletion. WineBridge-backed control calls share that coordination.

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

#[cfg(test)]
mod tests;

/// Bottle-specific configuration. Execution settings live in [`State`].
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BottleData {
    pub(crate) name: String,
    pub(crate) backend: PrefixBackend,
    #[serde(default)]
    pub(crate) programs: HashMap<Uuid, ProgramSpec>,
}

/// An immutable snapshot of a bottle's complete published configuration.
pub type BottleState = State<BottleData>;

impl crate::environment::BackendSource for BottleData {
    fn backend(&self) -> PrefixBackend {
        self.backend
    }
}

impl State<BottleData> {
    /// Returns the display name.
    ///
    /// Names are not identities and need not be unique.
    pub fn name(&self) -> &str {
        &self.data.name
    }

    /// Iterates over registered programs in unspecified order.
    pub fn programs(&self) -> impl Iterator<Item = (Uuid, &ProgramSpec)> {
        self.data.programs.iter().map(|(id, launch)| (*id, launch))
    }

    /// Returns the backend fixed when this bottle was created.
    pub fn backend(&self) -> PrefixBackend {
        self.data.backend
    }

    /// Returns the registered program with identity `id`.
    pub fn program(&self, id: Uuid) -> Option<&ProgramSpec> {
        self.data.programs.get(&id)
    }
}

/// A live bottle handle. Clones share state and coordination; dropping a handle
/// does not stop Wine. Operations fail after deletion, including identity access.
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
    pub fn id(&self) -> Result<Uuid> {
        Ok(self.state()?.id())
    }
    pub fn state(&self) -> Result<Arc<BottleState>> {
        self.0.state()
    }

    /// Observe current state and later publications. Deletion ends the stream.
    pub fn watch(&self) -> impl Stream<Item = Arc<BottleState>> + Send + 'static + use<> {
        self.0.watch()
    }
}

impl Bottle {
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.0.dll_overrides()
    }
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        self.0.set_dll_override(dll.into(), mode)
    }
    pub fn unset_dll_override(&self, dll: impl Into<String>) -> Operation<()> {
        self.0.unset_dll_override(dll.into())
    }

    /// Resolve the registration under coordination before preparing or starting Wine.
    pub fn launch_program(&self, id: Uuid) -> Operation<u32> {
        self.0.launch(move |state| {
            let launch = state
                .program(id)
                .cloned()
                .ok_or(BottleError::ProgramNotFound(id))?;
            Ok((id, launch))
        })
    }
    /// Launch an unregistered definition with a caller-selected process-group UUID.
    pub fn launch(&self, id: Uuid, launch: ProgramSpec) -> Operation<u32> {
        self.0.launch(move |_| Ok((id, launch)))
    }
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.0.processes().await
    }
    /// Terminate a process group without requiring a saved program registration
    /// or starting a stopped runtime.
    pub async fn kill_program(&self, id: Uuid) -> Result<()> {
        self.0.kill(|_| Ok(id)).await
    }
    pub async fn stop(&self) -> Result<()> {
        self.0.stop().await
    }
}

impl Bottle {
    /// Edit a private draft, validating the final state and applying software changes
    /// before saving and publishing once. Software changes require a stopped bottle;
    /// metadata and startup settings can change while running. Unchanged drafts
    /// return the callback's result without saving or publishing.
    pub fn edit<R: Send + 'static>(
        &self,
        callback: impl FnOnce(&mut Edit<'_, BottleData>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        self.0.edit(callback)
    }
}

impl Edit<'_, BottleData> {
    pub fn rename(&mut self, name: impl Into<String>) {
        self.draft.data.name = name.into();
    }

    pub fn add_program(&mut self, launch: ProgramSpec) -> Uuid {
        let id = Uuid::new_v4();
        self.draft.data.programs.insert(id, launch);
        id
    }

    pub fn remove_program(&mut self, id: Uuid) -> Option<ProgramSpec> {
        self.draft.data.programs.remove(&id)
    }

    /// Edit an existing launch definition without changing its registration ID.
    pub fn program(&mut self, id: Uuid) -> Option<&mut ProgramSpec> {
        self.draft.data.programs.get_mut(&id)
    }
}

#[cfg(feature = "fvs")]
impl Bottle {
    /// Stop the environment and capture its managed files and `state.toml`.
    /// Shared artifacts and external files are not copied or rebuilt on restoration.
    /// Explicit snapshots create a revision even without changes.
    /// The internal checkpoint message is reserved. Once capture starts it finishes
    /// under coordination, including when explicit cancellation is requested.
    pub fn create_snapshot(&self, message: impl Into<String>) -> Operation<Snapshot> {
        self.0.create_snapshot(message.into())
    }

    /// List newest-first user snapshots, excluding internal checkpoints.
    /// A bottle without history returns an empty list without contacting FVS.
    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        self.0.snapshots().await
    }

    /// Restore the complete managed root and publish its restored BottleState.
    /// UUID, backend and configuration format must match. A failed restore or
    /// invalid state recovers the previous files before returning; failed recovery
    /// reports the root requiring repair. Cancellation does not interrupt an active
    /// restore or recovery. Working files change without moving FVS's current commit.
    pub fn rollback(&self, revision: &str) -> Operation<String> {
        self.0.rollback(revision)
    }
}

/// Bottle-specific failures carried by [`crate::error::Error::Bottle`].
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
    /// Creates a bottle using `runner` and the selected storage strategy.
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
    /// # Errors
    ///
    /// Returns [`crate::EnvironmentError::RequiresAddon`] for missing coexistence
    /// requirements before creating any files. Other service, I/O, and prefix
    /// creation failures are returned directly.
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
