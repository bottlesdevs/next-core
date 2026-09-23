//! Bottle collection lifecycle backed by the shared environment registry.
use super::{Bottle, BottleData};
#[cfg(feature = "fvs")]
use crate::environment::VirgoManager;
use crate::{
    Addon, Component, Context, LibraryEntry, LibraryProvider, Operation, PrefixBackend,
    environment::Manager,
    error::{Error, Result},
};
use futures_core::Stream;
use futures_util::StreamExt;
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

/// The collection-level interface for bottles owned by one [`crate::Bottles`]
/// context.
///
/// Obtain this manager from [`crate::Bottles::bottles`]. Use it for
/// collection-level work—creating, opening, deleting, listing, and watching
/// bottles—then use the returned [`Bottle`] handles for operations on an
/// individual bottle.
///
/// Clones share a registry. Opening the same UUID through clones returns a
/// handle to the same live bottle state. The registry is loaded once from
/// library-managed storage and is updated by manager operations; it is not a
/// live view of external filesystem changes.
#[derive(Clone)]
pub struct BottleManager(Arc<Manager<BottleData>>);

#[async_trait::async_trait]
impl LibraryProvider for BottleManager {
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

impl BottleManager {
    #[cfg(test)]
    pub(crate) fn new(context: Context, #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>) -> Self {
        Self(Manager::new(
            context.directories().bottles(),
            context,
            #[cfg(feature = "fvs")]
            virgo,
        ))
    }

    /// Populates the shared collection, returning bottle configuration failures.
    pub(crate) async fn load(
        context: Context,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Result<Self> {
        Ok(Self(
            Manager::load(
                context.directories().bottles(),
                context,
                #[cfg(feature = "fvs")]
                virgo,
            )
            .await?,
        ))
    }

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
        self.0
            .create(
                BottleData {
                    name: name.into(),
                    backend,
                    programs: HashMap::new(),
                },
                runner,
                winebridge,
                umu,
            )
            .map(Bottle)
    }

    /// Stops and permanently deletes the bottle identified by `id`.
    ///
    /// Cancellation is observed after stopping and before withdrawal into trash.
    /// Once withdrawn, deletion is published and cleanup is best effort.
    ///
    /// After successful deletion, existing [`Bottle`] handles report deletion
    /// and their state streams end. Previously obtained [`crate::BottleState`]
    /// snapshots remain usable. Failed withdrawal leaves the registry unchanged;
    /// trash cleanup errors cannot invalidate deletion.
    ///
    /// # Errors
    ///
    /// The operation fails if the bottle does not exist, cannot be stopped,
    /// cancellation is requested, or its root cannot be moved into trash.
    pub fn delete(&self, id: Uuid) -> Operation<()> {
        self.0.delete(id)
    }

    /// Looks up the bottle identified by `id` synchronously in the registry.
    ///
    /// Repeated calls through this manager or its clones return handles to the
    /// same live state. Only bottles loaded at startup or created through this
    /// manager are opened; this method does not search storage or observe
    /// external changes.
    ///
    /// # Errors
    ///
    /// Returns [`crate::EnvironmentError::NotFound`] if `id` is not in the registry.
    pub fn open(&self, id: Uuid) -> Result<Bottle> {
        self.0.open(id).map(Bottle)
    }

    /// Returns the bottles currently known to this manager.
    ///
    /// This allocates a new vector of cloned handles; changes to bottle
    /// configuration do not change registry membership.
    ///
    /// The order is unspecified and must not be used as an identity or stable
    /// presentation order.
    pub fn list(&self) -> Vec<Bottle> {
        self.0.list().into_iter().map(Bottle).collect()
    }

    /// Watches this manager and every bottle currently registered in it.
    ///
    /// The stream first yields the current list, then the latest list after
    /// each observed membership or bottle-state change. New bottle streams are
    /// added as membership changes, and deleted bottle streams end with their
    /// bottle tombstones. Slow consumers may miss intermediate states.
    ///
    /// List order is unspecified. The stream ends when all manager handles for
    /// this context are dropped.
    pub fn watch(&self) -> impl Stream<Item = Vec<Bottle>> + Send + 'static + use<> {
        self.0
            .watch()
            .map(|environments| environments.into_iter().map(Bottle).collect())
    }
}
