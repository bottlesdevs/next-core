//! Live collections of persisted environments.
//!
//! A [`Manager`] is loaded once by [`crate::Bottles`] and then updated by create,
//! edit, rollback, and delete operations. It does not poll the filesystem for
//! changes made outside this crate.

use crate::environment::Inner;
#[cfg(feature = "fvs")]
use crate::environment::VirgoManager;
use crate::{
    Addon, Context, Environment, EnvironmentError, EnvironmentState, Operation, PrefixBackend,
    Progress, Runner, Stage, Umu, WineBridge, error::Result,
};
use futures_core::Stream;
use futures_util::{
    StreamExt,
    stream::{self, SelectAll},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
};
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;
use uuid::Uuid;

type Members = Arc<HashMap<Uuid, Environment>>;

/// Tracks the live collection of environments.
///
/// Obtain it from [`crate::Bottles::environments`]. Clones share collection
/// membership. Opening the same UUID through clones
/// returns handles to the same live state. The collection is loaded once from
/// library-managed storage and updated by manager operations; it does not
/// observe external filesystem changes.
#[derive(Clone)]
pub struct Manager {
    root: PathBuf,
    context: Context,
    #[cfg(feature = "fvs")]
    virgo: Arc<VirgoManager>,
    published: watch::Sender<Members>,
}
enum Event {
    Membership(Members),
    Changed,
}
type Events = Pin<Box<dyn Stream<Item = Option<Event>> + Send>>;

impl Manager {
    /// Returns cloned handles for the currently known members.
    /// Configuration changes do not change collection membership.
    /// Order is unspecified and must not be used as an identity or stable
    /// presentation order.
    pub fn list(&self) -> Vec<Environment> {
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
    pub fn open(&self, id: Uuid) -> Result<Environment> {
        self.published
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| EnvironmentError::NotFound(id).into())
    }

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
            let environment = Environment(Inner::from_state(
                state,
                root,
                manager.context.clone(),
                #[cfg(feature = "fvs")]
                manager.virgo.clone(),
            )?);
            members.insert(environment.id(), environment);
        }
        manager.published.send_replace(Arc::new(members));
        Ok(manager)
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
            let id = Uuid::new_v4();
            let state = EnvironmentState::new(backend, runner, winebridge, umu);
            let environment = Inner::create(
                state,
                manager.root.join(id.to_string()),
                manager.context.clone(),
                #[cfg(feature = "fvs")]
                manager.virgo.clone(),
                &progress,
                &cancellation,
            )
            .await?;
            let handle = Environment(environment);
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
                .0
                .delete(&progress, &cancellation, || {
                    manager.published.send_modify(|published| {
                        Arc::make_mut(published).remove(&id);
                    });
                })
                .await
        })
    }

    /// Observes collection membership and member state changes, including edits
    /// and rollback. First yields the current list, then the latest list after
    /// each observed change. Slow consumers may miss intermediate states.
    ///
    /// Each list contains live handles, not state snapshots from the time of the
    /// event. Order is unspecified. The stream ends when all manager clones and
    /// operations retaining this collection are dropped, even if item handles
    /// remain alive.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Manager;
    /// # use futures_lite::StreamExt;
    /// # async fn observe(manager: &Manager) {
    /// let updates = manager.watch();
    /// futures_lite::pin!(updates);
    /// if let Some(environments) = updates.next().await {
    ///     println!("{} environments", environments.len());
    /// }
    /// # }
    /// ```
    pub fn watch(&self) -> impl Stream<Item = Vec<Environment>> + Send + 'static + use<> {
        let published = self.published.subscribe();
        let mut events = SelectAll::<Events>::new();
        // End the aggregate when the manager closes, even if callers retain handles.
        events.push(Box::pin(
            WatchStream::new(published.clone())
                .map(|members| Some(Event::Membership(members)))
                .chain(stream::once(async { None })),
        ));
        stream::unfold(
            (published, events, HashSet::new()),
            |(published, mut events, mut subscribed)| async move {
                let event = events.next().await??;
                match event {
                    Event::Membership(members) => {
                        subscribed.retain(|id| members.contains_key(id));
                        for (id, handle) in members.iter() {
                            if subscribed.insert(*id) {
                                let environment = &handle.0;
                                let mut previous = environment.state().ok();
                                events.push(Box::pin(environment.watch().filter_map(
                                    move |state| {
                                        let changed = previous
                                            .as_ref()
                                            .is_none_or(|current| !Arc::ptr_eq(current, &state));
                                        previous = Some(state);
                                        std::future::ready(changed.then_some(Some(Event::Changed)))
                                    },
                                )));
                            }
                        }
                        let list = members.values().cloned().collect();
                        Some((list, (published, events, subscribed)))
                    }
                    Event::Changed => {
                        let list = published.borrow().values().cloned().collect();
                        Some((list, (published, events, subscribed)))
                    }
                }
            },
        )
    }
}
