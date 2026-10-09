//! Live collections of persisted environments.
//!
//! A [`Manager`] is loaded once by [`crate::Bottles`] and then updated by create,
//! edit, rollback, and delete operations. It does not poll the filesystem for
//! changes made outside this crate.

#[cfg(feature = "fvs")]
use crate::environment::VirgoManager;
use crate::environment::{BackendSource, Environment, State};
use crate::{
    Addon, Context, EnvironmentConfig, EnvironmentError, Operation, Progress, Runner, Stage, Umu,
    WineBridge, error::Result, utils::join::join,
};
use futures_core::Stream;
use futures_util::StreamExt;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::watch;
use uuid::Uuid;

pub(crate) trait Managed {
    type Data;

    fn from_environment(environment: Arc<Environment<Self::Data>>) -> Self;
    fn environment(&self) -> &Environment<Self::Data>;
}

type Members<T> = Arc<HashMap<Uuid, T>>;

/// Tracks the live collection of bottles or standalone programs.
///
/// Obtain `Manager<Bottle>` from [`crate::Bottles::bottles`], or
/// `Manager<Program>` from `Bottles::programs` with the `fvs` feature enabled.
/// Clones share collection membership. Opening the same UUID through clones
/// returns handles to the same live state. The collection is loaded once from
/// library-managed storage and updated by manager operations; it does not
/// observe external filesystem changes.
#[derive(Clone)]
pub struct Manager<T> {
    root: PathBuf,
    context: Context,
    #[cfg(feature = "fvs")]
    virgo: Arc<VirgoManager>,
    published: watch::Sender<Members<T>>,
}

impl<T: Clone> Manager<T> {
    /// Returns cloned handles for the currently known members.
    /// Configuration changes do not change collection membership.
    /// Order is unspecified and must not be used as an identity or stable
    /// presentation order.
    pub fn list(&self) -> Vec<T> {
        self.published.borrow().values().cloned().collect()
    }

    /// Looks up a member without filesystem or runtime work.
    /// Repeated calls through this manager or its clones return handles to the
    /// same live state. Only members loaded at startup or created through this
    /// manager are opened.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::NotFound`] if `id` is not in the collection.
    pub fn open(&self, id: Uuid) -> Result<T> {
        self.published
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| EnvironmentError::NotFound(id).into())
    }
}

// Only core handles supply environment access; the adapter stays private.
#[allow(private_bounds)]
impl<T, Data> Manager<T>
where
    T: Managed<Data = Data> + Clone + Send + Sync + 'static,
    Data: BackendSource + Send,
    State<Data>: next_config::Config + Clone + PartialEq + Send + Sync,
{
    pub(crate) fn new(
        root: PathBuf,
        context: Context,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Self {
        Self {
            root,
            context,
            #[cfg(feature = "fvs")]
            virgo,
            published: watch::channel(Arc::new(HashMap::new())).0,
        }
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
        let manager = Self::new(
            root,
            context,
            #[cfg(feature = "fvs")]
            virgo,
        );
        let mut members = HashMap::new();
        let mut entries = match async_fs::read_dir(&manager.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(manager),
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next().await {
            let root = entry?.path();
            let file = root.join("state.toml");
            let state: State<T::Data> = match next_config::load(&file).await {
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
            let id = state.id();
            let environment = Environment::from_state(
                state,
                root,
                manager.context.clone(),
                #[cfg(feature = "fvs")]
                manager.virgo.clone(),
            )?;
            members.insert(id, T::from_environment(environment));
        }
        manager.published.send_replace(Arc::new(members));
        Ok(manager)
    }

    /// Creates and publishes one environment as a lazy operation.
    ///
    /// The generated UUID is not added to collection membership until backend
    /// initialization and state persistence have completed successfully.
    pub(crate) fn create_environment(
        &self,
        data: T::Data,
        runner: Addon<Runner>,
        winebridge: Addon<WineBridge>,
        umu: Option<Addon<Umu>>,
    ) -> Operation<T> {
        let manager = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let id = Uuid::new_v4();
            let state = State {
                id,
                config: EnvironmentConfig::new(runner, winebridge, umu),
                data,
            };
            let environment = Environment::create(
                state,
                manager.root.join(id.to_string()),
                manager.context.clone(),
                #[cfg(feature = "fvs")]
                manager.virgo.clone(),
                &progress,
                &cancellation,
            )
            .await?;
            let handle = T::from_environment(environment);
            manager.published.send_modify(|published| {
                Arc::make_mut(published).insert(id, handle.clone());
            });
            Ok(handle)
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
                .environment()
                .delete(&progress, &cancellation, || {
                    manager.published.send_modify(|published| {
                        Arc::make_mut(published).remove(&id);
                    });
                })
                .await
        })
    }

    /// Observes collection membership and member state changes, including edits
    /// and rollback. First yields current state snapshots, then the latest list
    /// after each observed change. Slow consumers may miss intermediate states.
    ///
    /// Previously emitted snapshots remain unchanged. Use [`open`](Self::open)
    /// with a state's ID to obtain a handle when an action is needed.
    /// Order is unspecified. The stream ends when all manager clones and
    /// operations retaining this collection are dropped, even if item handles
    /// remain alive.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Bottle, Manager};
    /// # use futures_lite::StreamExt;
    /// # async fn observe(manager: &Manager<Bottle>) {
    /// let updates = manager.watch();
    /// futures_lite::pin!(updates);
    /// if let Some(states) = updates.next().await {
    ///     for state in states {
    ///         println!("{}: {}", state.id(), state.name());
    ///     }
    /// }
    /// # }
    /// ```
    pub fn watch(
        &self,
    ) -> impl Stream<Item = Vec<Arc<State<Data>>>> + Send + 'static + use<T, Data> {
        join(self.published.subscribe(), |handle| {
            handle.environment().watch().boxed()
        })
        .map(|states| states.values().cloned().collect())
    }
}
