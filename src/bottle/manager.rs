//! Collection lifecycle and discovery for library-managed bottles.

use std::{
    collections::{HashMap, HashSet},
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

#[cfg(feature = "fvs")]
use crate::prefix::FVS_BLOCK_SIZE;
use crate::{
    Context, Operation, Progress, Stage,
    addons::{Addon, Addons, Requirement, Slot},
    error::{Error, Result},
    prefix::Prefix,
};

use super::{
    error::BottleError,
    state::{Bottle, BottleState, Storage},
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
#[derive(Clone)]
pub struct BottleManager {
    pub(super) context: Context,
    pub(super) addons: Addons,
    registry: Arc<BottleRegistry>,
}

impl BottleManager {
    pub(crate) fn new(context: Context, addons: Addons) -> Self {
        Self {
            context,
            addons,
            registry: Arc::new(BottleRegistry::new()),
        }
    }

    /// Populates the shared registry, skipping unreadable bottle configuration
    /// with a warning so one corrupt bottle does not prevent startup.
    pub(crate) async fn load(context: Context, addons: Addons) -> Result<Self> {
        let manager = Self::new(context, addons);
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
    /// runner component. With the default `fvs` feature, creation requires the
    /// configured FVS service even for [`Storage::Standard`]. Failures, and
    /// cancellation observed while the operation remains polled, remove the
    /// partially-created bottle directory on a best-effort basis. Dropping a
    /// started operation or a cleanup failure can leave a directory that a
    /// later library startup discovers.
    ///
    /// # Errors
    ///
    /// Returns [`BottleError::RequiresAddon`] with every missing runtime
    /// requirement before creating any files. Other service, I/O, and prefix
    /// creation failures are returned directly.
    pub fn create(
        &self,
        name: impl Into<String>,
        storage: Storage,
        runner: Uuid,
    ) -> Operation<Bottle> {
        let name = name.into();
        let cx = self.context.clone();
        let addons = self.addons.clone();
        let registry = self.registry.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let runner_component = addons
                .component(runner)
                .ok_or(crate::AddonError::NotFound(runner))?;
            if runner_component.slot() != Slot::Runner {
                return Err(BottleError::InvalidComponentSlot {
                    component: runner_component.id(),
                    required: Slot::Runner,
                }
                .into());
            }
            let winebridge = addons.latest_component(Slot::WineBridge);
            let needs_umu = runner_component
                .requirements()
                .contains(&Requirement::Slot(Slot::Umu));
            let umu = needs_umu
                .then(|| addons.latest_component(Slot::Umu))
                .flatten();
            let mut missing = Vec::new();
            if winebridge.is_none() {
                missing.push(Requirement::Slot(Slot::WineBridge));
            }
            if needs_umu && umu.is_none() {
                missing.push(Requirement::Slot(Slot::Umu));
            }
            if !missing.is_empty() {
                return Err(BottleError::RequiresAddon {
                    required_by: None,
                    requirements: missing,
                }
                .into());
            }
            let winebridge = winebridge.unwrap(); // Safe to unwrap since we just checked it above
            let loaded_runner = runner_component
                .load_runner(cx.directories(), umu.as_deref())
                .await?;
            let id = Uuid::new_v4();
            let bottle_path = cx.directories().bottle(id);
            fs::create_dir_all(&bottle_path).await?;

            let result = async {
                progress.send_replace(Some(Progress::new(Stage::CreatingPrefix)));
                let storage = Prefix::create(
                    storage,
                    &bottle_path,
                    loaded_runner.as_ref(),
                    &runner_component.id().to_string(),
                    &cx,
                )
                .await?;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }

                let mut components = HashMap::from([
                    (Slot::WineBridge, Addon::from(winebridge.as_ref())),
                    (Slot::Runner, Addon::from(runner_component.as_ref())),
                ]);
                if let Some(umu) = umu {
                    components.insert(Slot::Umu, Addon::from(umu.as_ref()));
                }
                let bottle = Bottle::new(
                    id,
                    name,
                    components,
                    Vec::new(),
                    storage,
                    cx.clone(),
                    addons.clone(),
                )
                .await?;
                progress.send_replace(Some(Progress::new(Stage::Configuring)));
                #[cfg(feature = "fvs")]
                cx.fvs()
                    .await?
                    .new_repository(&bottle_path, FVS_BLOCK_SIZE)
                    .await?;
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
            let _write = bottle.0.write_lock.write().await;
            let state = bottle.state()?;
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            Bottle::stop_state(&state, &bottle.0.cx).await?;
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
    /// same live state. Once a UUID is in the registry, this method does not
    /// reload `bottle.toml` or observe external changes. If a persisted bottle
    /// is not yet interned, opening it adds the handle to the registry and
    /// notifies manager watchers.
    ///
    /// # Errors
    ///
    /// Returns [`BottleError::NotFound`] if `bottle.toml` is absent, is not a
    /// regular file, or its metadata cannot be inspected. Returns
    /// [`BottleError::IdMismatch`] if the loaded UUID differs from `id`.
    /// Configuration loading failures are also returned.
    pub async fn open(&self, id: Uuid) -> Result<Bottle> {
        if let Some(bottle) = self.registry.get(id) {
            return Ok(bottle);
        }
        let path = self.context.directories().bottle(id).join("bottle.toml");
        if !fs::metadata(&path).await.is_ok_and(|entry| entry.is_file()) {
            return Err(BottleError::NotFound(id).into());
        }
        let state: BottleState = next_config::load(path).await?;
        if state.id != id {
            return Err(BottleError::IdMismatch {
                expected: id,
                actual: state.id,
            }
            .into());
        }
        let bottle = Bottle::from_state(state, self.context.clone(), self.addons.clone())?;
        Ok(self.registry.intern(bottle))
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
    pub fn watch(&self) -> impl Stream<Item = Vec<Bottle>> + Send + 'static {
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
                    match Bottle::from_state(state, self.context.clone(), self.addons.clone()) {
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
