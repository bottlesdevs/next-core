//! Collection lifecycle and discovery for library-managed bottles.

#[cfg(feature = "fvs")]
use crate::environment::VirgoManager;

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    io,
    pin::Pin,
    sync::Arc,
};

use async_fs as fs;
use futures_core::Stream;
use futures_lite::{StreamExt, stream};
use futures_util::stream::SelectAll;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;
use uuid::Uuid;

use crate::{
    Context, EnvironmentConfig, Operation, PrefixBackend, Progress, Stage,
    addons::Addons,
    environment,
    error::{Error, Result},
};

use super::{
    error::BottleError,
    state::{Bottle, BottleState},
};

/// The shared membership registry behind [`BottleManager`] clones.
///
/// It interns one live [`Bottle`] handle per UUID and publishes copy-on-write
/// snapshots to manager watchers.
struct BottleRegistry(watch::Sender<Arc<HashMap<Uuid, Bottle>>>);

enum BottleManagerEvent {
    Membership(Arc<HashMap<Uuid, Bottle>>),
    BottleChanged,
}

type BottleManagerEventStream = Pin<Box<dyn Stream<Item = BottleManagerEvent> + Send>>;

impl BottleRegistry {
    fn new() -> Self {
        let (published, _) = watch::channel(Arc::new(HashMap::new()));
        Self(published)
    }

    fn list(&self) -> Vec<Bottle> {
        self.0.borrow().values().cloned().collect()
    }

    fn get(&self, id: Uuid) -> Option<Bottle> {
        self.0.borrow().get(&id).cloned()
    }

    fn replace(&self, bottles: Vec<Bottle>) {
        self.0.send_replace(Arc::new(
            bottles
                .into_iter()
                .map(|bottle| (bottle.0.id, bottle))
                .collect(),
        ));
    }

    fn intern(&self, bottle: Bottle) -> Bottle {
        let mut interned = bottle.clone();
        self.0.send_if_modified(|published| {
            if let Some(current) = published.get(&bottle.0.id) {
                interned = current.clone();
                return false;
            }
            let mut bottles = published.as_ref().clone();
            bottles.insert(bottle.0.id, bottle);
            *published = Arc::new(bottles);
            true
        });
        interned
    }

    fn remove(&self, id: Uuid) {
        self.0.send_if_modified(|published| {
            let mut bottles = published.as_ref().clone();
            if bottles.remove(&id).is_none() {
                return false;
            }
            *published = Arc::new(bottles);
            true
        });
    }
}

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
/// Hashing identifies that shared registry and remains stable as bottles change.
#[derive(Clone)]
pub struct BottleManager {
    pub(super) context: Context,
    pub(super) addons: Addons,
    #[cfg(feature = "fvs")]
    virgo: Arc<VirgoManager>,
    registry: Arc<BottleRegistry>,
}

// Allows the live handle to key iced subscriptions directly: clones must hash
// alike, while registry changes must not restart the subscription.
impl Hash for BottleManager {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.registry).hash(state);
    }
}

impl BottleManager {
    pub(crate) fn new(
        context: Context,
        addons: Addons,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Self {
        Self {
            context,
            addons,
            #[cfg(feature = "fvs")]
            virgo,
            registry: Arc::new(BottleRegistry::new()),
        }
    }

    /// Populates the shared registry, skipping unreadable bottle configuration
    /// with a warning so one corrupt bottle does not prevent startup.
    pub(crate) async fn load(
        context: Context,
        addons: Addons,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Result<Self> {
        let manager = Self::new(
            context,
            addons,
            #[cfg(feature = "fvs")]
            virgo,
        );
        let bottles = manager.load_bottles().await?;
        manager.registry.replace(bottles);
        Ok(manager)
    }

    /// Creates a bottle using `runner` and the selected storage strategy.
    ///
    /// A new UUID is assigned when the operation starts;
    /// display names are stored verbatim, may be empty, and need not be unique.
    /// The newest downloaded WineBridge is selected automatically. A runner
    /// requiring UMU also receives the newest downloaded UMU release. No addon
    /// is downloaded implicitly. The runner UUID must identify a downloaded
    /// runner component. Standard creation initializes Wine without FVS. Virgo
    /// creation only saves selections and creates private storage directories;
    /// artifacts and registry data are prepared before startup. Failures, and
    /// cancellation observed while the operation remains polled, remove the
    /// partially-created bottle directory on a best-effort basis. Dropping a
    /// started operation or a cleanup failure can leave a directory that a
    /// later library startup discovers.
    ///
    /// # Errors
    ///
    /// Returns [`crate::EnvironmentError::RequiresAddon`] with every missing runtime
    /// requirement before creating any files. Other service, I/O, and prefix
    /// creation failures are returned directly.
    pub fn create(
        &self,
        name: impl Into<String>,
        backend: PrefixBackend,
        runner: Uuid,
    ) -> Operation<Bottle> {
        let name = name.into();
        let cx = self.context.clone();
        let addons = self.addons.clone();
        #[cfg(feature = "fvs")]
        let virgo = self.virgo.clone();
        let registry = self.registry.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let id = Uuid::new_v4();
            let bottle_path = cx.directories().bottle(id);
            // Initialization may retain live storage on failure; only remove after it succeeds.
            let config = EnvironmentConfig::new(backend, runner, &addons)?;
            environment::initialize(&config, &bottle_path, &cx, &progress, &cancellation).await?;
            let result = async {
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let bottle = Bottle::new(
                    id,
                    name,
                    config,
                    cx.clone(),
                    addons.clone(),
                    #[cfg(feature = "fvs")]
                    virgo.clone(),
                )
                .await?;
                progress.send_replace(Some(Progress::new(Stage::Configuring)));
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let bottle = registry.intern(bottle);
                Ok(bottle)
            }
            .await;

            if result.is_err() {
                let _ = fs::remove_dir_all(bottle_path).await;
            }
            result
        })
    }

    /// Stops and permanently deletes the bottle identified by `id`.
    ///
    /// Cancellation is observed after stopping and before recursive removal
    /// starts; removal itself is not cancellable.
    ///
    /// After successful deletion, existing [`Bottle`] handles report deletion
    /// and their state streams end. Previously obtained [`BottleState`]
    /// snapshots remain usable. The registry is changed only after recursive
    /// removal succeeds; partial filesystem removal is not rolled back.
    ///
    /// # Errors
    ///
    /// The operation fails if the bottle does not exist, cannot be stopped,
    /// cancellation is requested, or its files cannot be removed.
    pub fn delete(&self, id: Uuid) -> Operation<()> {
        let manager = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let bottle = manager.open(id).await?;
            let _control = cancellation
                .run_until_cancelled(bottle.0.control.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            bottle.stop_locked().await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.send_replace(Some(Progress::new(Stage::Removing)));
            let path = manager.context.directories().bottle(id);
            fs::remove_dir_all(path).await?;
            manager.registry.remove(id);
            bottle.mark_deleted();
            Ok(())
        })
    }

    /// Opens the bottle identified by `id`.
    ///
    /// Repeated calls through this manager or its clones return handles to the
    /// same live state. Only bottles loaded at startup or created through this
    /// manager are opened; this method does not search storage or observe
    /// external changes.
    ///
    /// # Errors
    ///
    /// Returns [`BottleError::NotFound`] if `id` is not in the registry.
    pub async fn open(&self, id: Uuid) -> Result<Bottle> {
        self.registry
            .get(id)
            .ok_or_else(|| BottleError::NotFound(id).into())
    }

    /// Returns the bottles currently known to this manager.
    ///
    /// This allocates a new vector of cloned handles; changes to bottle
    /// configuration do not change registry membership.
    ///
    /// The order is unspecified and must not be used as an identity or stable
    /// presentation order.
    pub fn list(&self) -> Vec<Bottle> {
        self.registry.list()
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
        let mut events = SelectAll::<BottleManagerEventStream>::new();
        events.push(Box::pin(
            WatchStream::new(self.registry.0.subscribe()).map(BottleManagerEvent::Membership),
        ));

        stream::unfold(
            (self.clone(), events, HashSet::new()),
            |(manager, mut events, mut subscribed)| async move {
                match events.next().await? {
                    BottleManagerEvent::Membership(bottles) => {
                        subscribed.retain(|id| bottles.contains_key(id));
                        for (id, bottle) in bottles.iter() {
                            if subscribed.insert(*id) {
                                let mut previous = bottle.state().ok();
                                events.push(Box::pin(bottle.watch().filter_map(move |state| {
                                    let changed = previous
                                        .as_ref()
                                        .is_none_or(|current| !Arc::ptr_eq(current, &state));
                                    previous = Some(state);
                                    changed.then_some(BottleManagerEvent::BottleChanged)
                                })));
                            }
                        }
                        let bottles = bottles.values().cloned().collect();
                        Some((bottles, (manager, events, subscribed)))
                    }
                    BottleManagerEvent::BottleChanged => {
                        let bottles = manager.list();
                        Some((bottles, (manager, events, subscribed)))
                    }
                }
            },
        )
    }

    async fn load_bottles(&self) -> Result<Vec<Bottle>> {
        let bottles_path = self.context.directories().bottles();
        let mut entries = match fs::read_dir(bottles_path).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut paths = Vec::new();
        while let Some(entry) = entries.try_next().await? {
            let path = entry.path().join("bottle.toml");
            if fs::metadata(&path).await.is_ok_and(|entry| entry.is_file()) {
                paths.push(path);
            }
        }
        let mut bottles = Vec::with_capacity(paths.len());
        for path in paths {
            match next_config::load::<BottleState>(path).await {
                Ok(state) => {
                    match Bottle::from_state(
                        state,
                        self.context.clone(),
                        self.addons.clone(),
                        #[cfg(feature = "fvs")]
                        self.virgo.clone(),
                    ) {
                        Ok(bottle) => bottles.push(bottle),
                        Err(error) => {
                            tracing::warn!("skipping bottle with invalid runtime: {error}")
                        }
                    }
                }
                Err(error) => tracing::warn!("skipping unreadable bottle: {error}"),
            }
        }
        Ok(bottles)
    }
}
