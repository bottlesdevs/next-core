//! Bottle collection lifecycle backed by the shared environment registry.
use super::{Bottle, BottleError, BottleState};
#[cfg(feature = "fvs")]
use crate::environment::VirgoManager;
use crate::{
    Addons, Context, EnvironmentState, Operation, PrefixBackend, Progress, Stage,
    environment::{Environment, Registry},
    error::Result,
};
use futures_core::Stream;
use futures_util::StreamExt;
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};
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
/// Hashing identifies that shared registry and remains stable as bottles change.
#[derive(Clone)]
pub struct BottleManager {
    pub(super) context: Context,
    pub(super) addons: Addons,
    #[cfg(feature = "fvs")]
    virgo: Arc<VirgoManager>,
    registry: Arc<Registry<BottleState>>,
}

// Allows the live handle to key iced subscriptions directly: clones must hash
// alike, while registry changes must not restart the subscription.
impl Hash for BottleManager {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.registry).hash(state);
    }
}

impl BottleManager {
    #[cfg(test)]
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
            registry: Arc::new(Registry::new()),
        }
    }

    /// Populates the shared registry, skipping unreadable bottle configuration
    /// with a warning so one corrupt bottle does not prevent startup.
    pub(crate) async fn load(
        context: Context,
        addons: Addons,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Result<Self> {
        let registry = Arc::new(
            Registry::load(
                &context.directories().bottles(),
                &context,
                &addons,
                #[cfg(feature = "fvs")]
                &virgo,
            )
            .await?,
        );
        Ok(Self {
            context,
            addons,
            registry,
            #[cfg(feature = "fvs")]
            virgo,
        })
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
            let state = BottleState {
                id,
                name,
                backend,
                environment: EnvironmentState::new(runner, &addons)?,
                programs: HashMap::new(),
            };
            let environment = Environment::create(
                state,
                bottle_path,
                cx,
                addons,
                #[cfg(feature = "fvs")]
                virgo,
                &progress,
                &cancellation,
            )
            .await?;
            Ok(Bottle(registry.intern(id, environment)))
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
            bottle.0.delete(&progress, &cancellation).await?;
            manager.registry.remove(id);
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
            .map(Bottle)
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
        self.registry.list().into_iter().map(Bottle).collect()
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
        self.registry
            .watch()
            .map(|environments| environments.into_iter().map(Bottle).collect())
    }
}
