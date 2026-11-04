//! Live collections of persisted environments.
//!
//! A [`Manager`] is loaded once by [`crate::Bottles`] and then updated by create,
//! edit, rollback, and delete operations. It does not poll the filesystem for
//! changes made outside this crate.

#[cfg(feature = "fvs")]
use crate::environment::VirgoManager;
use crate::environment::{Inner, Inventory};
use crate::{
    Addon, Context, Environment, EnvironmentError, EnvironmentState, Operation, PrefixBackend,
    Progress, Runner, Stage, Umu, WineBridge, error::Result,
};
use futures_core::Stream;
use futures_util::StreamExt;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;
use uuid::Uuid;

/// Tracks the live collection of environments.
///
/// Obtain it from [`crate::Bottles::environments`]. Clones share collection
/// membership and its [`Inventory`]. Opening the same UUID through clones
/// returns handles to the same live state. The collection is loaded once from
/// library-managed storage and updated by manager operations; it does not
/// observe external filesystem changes.
#[derive(Clone)]
pub struct Manager {
    root: PathBuf,
    context: Context,
    #[cfg(feature = "fvs")]
    virgo: Arc<VirgoManager>,
    inventory: watch::Sender<Inventory>,
    members: Arc<Mutex<HashMap<Uuid, Environment>>>,
}

impl Manager {
    /// Returns cloned handles for the currently known members.
    ///
    /// Order is unspecified and must not be used as an identity or stable
    /// presentation order.
    pub fn list(&self) -> Vec<Environment> {
        self.members.lock().unwrap().values().cloned().collect()
    }

    /// Looks up a member without filesystem or runtime work.
    ///
    /// Repeated calls through this manager or its clones return handles to the
    /// same live state. Only members loaded at startup or created through this
    /// manager are opened.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::NotFound`] if `id` is not in the collection.
    pub fn open(&self, id: Uuid) -> Result<Environment> {
        self.members
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| EnvironmentError::NotFound(id).into())
    }

    /// Returns the current state of every environment, ordered by identifier.
    pub fn inventory(&self) -> Inventory {
        self.inventory.borrow().clone()
    }

    /// Observes the inventory, including creation, edits, rollback, and deletion.
    ///
    /// First yields the current inventory, then the latest one after each
    /// change. Slow consumers may miss intermediate inventories. The stream ends
    /// when the manager, its clones, and every environment handle are dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Manager;
    /// # use futures_lite::StreamExt;
    /// # async fn observe(manager: &Manager) {
    /// let updates = manager.watch();
    /// futures_lite::pin!(updates);
    /// if let Some(inventory) = updates.next().await {
    ///     println!("{} environments", inventory.len());
    /// }
    /// # }
    /// ```
    pub fn watch(&self) -> impl Stream<Item = Inventory> + Send + 'static + use<> {
        WatchStream::new(self.inventory.subscribe())
    }

    /// Loads valid UUID-named environments found below `root`.
    ///
    /// A missing root produces an empty manager. Entries without `state.toml`
    /// and non-directory entries are ignored; malformed or invalid persisted
    /// state aborts the entire load.
    ///
    /// # Errors
    ///
    /// Returns an error when directory enumeration, state loading, validation,
    /// or environment reconstruction fails.
    pub(crate) async fn load(
        root: PathBuf,
        context: Context,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Result<Self> {
        let manager = Self {
            root,
            context,
            #[cfg(feature = "fvs")]
            virgo,
            inventory: watch::channel(Arc::default()).0,
            members: Arc::default(),
        };
        let mut states = BTreeMap::new();
        let mut members = HashMap::new();
        let mut entries = match async_fs::read_dir(&manager.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(manager),
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next().await {
            let root = entry?.path();
            let file = root.join("state.toml");
            let state: EnvironmentState = match next_config::load(&file).await {
                Ok(state) => state,
                Err(next_config::error::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            state.validate()?;
            let environment = manager.environment(root)?;
            states.insert(environment.id(), Arc::new(state));
            members.insert(environment.id(), environment);
        }
        *manager.members.lock().unwrap() = members;
        manager.inventory.send_replace(Arc::new(states));
        Ok(manager)
    }

    fn environment(&self, root: PathBuf) -> Result<Environment> {
        Ok(Environment(Inner::new(
            root,
            self.inventory.clone(),
            self.context.clone(),
            #[cfg(feature = "fvs")]
            self.virgo.clone(),
        )?))
    }

    /// Creates an operation that initializes and persists an environment.
    ///
    /// A new UUID is assigned when the operation starts. Addon coexistence
    /// requirements are checked before creating files; missing requirements fail
    /// without selecting or downloading other addons. Standard creation
    /// initializes Wine without FVS. Virgo, available with the `fvs` feature,
    /// builds missing shared layers before composing private storage.
    /// The environment is added to this manager only after initialization and
    /// `state.toml` persistence succeed. Cancellation or persistence failure
    /// after backend initialization attempts best-effort removal; earlier
    /// backend failures, dropping a started operation, or cleanup failure can
    /// leave an unregistered directory that a later core startup may discover.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::RequiresAddon`] for missing coexistence
    /// requirements before creating any files. Awaiting the operation can also
    /// fail during cancellation, service startup, I/O, prefix initialization, or
    /// state persistence.
    pub fn create(
        &self,
        backend: PrefixBackend,
        runner: Addon<Runner>,
        winebridge: Addon<WineBridge>,
        umu: Option<Addon<Umu>>,
    ) -> Operation<Environment> {
        let manager = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let state = EnvironmentState::new(backend, runner, winebridge, umu);
            state.validate()?;
            let environment = manager.environment(manager.root.join(Uuid::new_v4().to_string()))?;
            environment
                .0
                .create(&state, &progress, &cancellation)
                .await?;
            // Join membership first so watchers can open what they observe.
            manager
                .members
                .lock()
                .unwrap()
                .insert(environment.id(), environment.clone());
            environment.0.publish(state);
            Ok(environment)
        })
    }

    /// Creates an operation that stops and removes a member.
    ///
    /// Cancellation is observed while waiting for coordination and again after
    /// stopping, before withdrawal into trash. Once stopping begins, shutdown is
    /// allowed to finish even if cancellation is requested.
    /// Once withdrawn, deletion and removal from the collection are published
    /// before best-effort cleanup. Existing handles report deletion and their
    /// state streams end; previously obtained state snapshots remain usable.
    /// A failure or cancellation before withdrawal leaves membership unchanged,
    /// but the runtime may already be stopped. Trash cleanup errors cannot
    /// invalidate a published deletion.
    ///
    /// # Errors
    ///
    /// Awaiting the operation fails if the member does not exist, cannot be
    /// stopped, observes cancellation, or cannot move its root into trash.
    pub fn delete(&self, id: Uuid) -> Operation<()> {
        let manager = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let handle = manager.open(id)?;
            handle
                .0
                .delete(&progress, &cancellation, || {
                    manager.members.lock().unwrap().remove(&id);
                })
                .await
        })
    }
}
