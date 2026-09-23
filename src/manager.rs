//! Shared environment collection lifecycle and observation.

#[cfg(feature = "fvs")]
use crate::environment::VirgoManager;
use crate::environment::{BackendSource, Environment, State};
use crate::{
    Addon, Component, Context, EnvironmentConfig, EnvironmentError, Operation, Progress, Stage,
    error::Result,
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

pub(crate) trait Managed {
    type Data;

    fn from_environment(environment: Arc<Environment<Self::Data>>) -> Self;
    fn environment(&self) -> &Environment<Self::Data>;
}

type Members<T> = Arc<HashMap<Uuid, T>>;

/// The collection-level interface for bottles or standalone programs owned by
/// one [`crate::Bottles`] context.
///
/// Obtain `Manager<Bottle>` from [`crate::Bottles::bottles`], or
/// `Manager<Program>` from `Bottles::programs` with the `fvs` feature enabled.
/// Use the manager to create, open, delete, list, and watch its members, then use
/// the returned handles for operations on an individual environment.
///
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
enum Event<T> {
    Membership(Members<T>),
    Changed,
}
type Events<T> = Pin<Box<dyn Stream<Item = Option<Event<T>>> + Send>>;

impl<T: Clone> Manager<T> {
    /// Returns the currently known members as a new vector of cloned handles.
    /// Configuration changes do not change collection membership.
    /// Order is unspecified and must not be used as an identity or stable
    /// presentation order.
    pub fn list(&self) -> Vec<T> {
        self.published.borrow().values().cloned().collect()
    }

    /// Looks up a member synchronously without filesystem or runtime work.
    /// Repeated calls through this manager or its clones return handles to the
    /// same live state. Only members loaded at startup or created through this
    /// manager are opened.
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
impl<T: Managed + Clone + Send + Sync + 'static> Manager<T>
where
    T::Data: BackendSource + Send,
    State<T::Data>: next_config::Config + Clone + PartialEq + Send + Sync,
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

    pub(crate) fn create_environment(
        &self,
        data: T::Data,
        runner: Addon<Component>,
        winebridge: Addon<Component>,
        umu: Option<Addon<Component>>,
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

    /// Stops and permanently deletes the member identified by `id`.
    ///
    /// Cancellation is observed after stopping and before withdrawal into trash.
    /// Once withdrawn, deletion and removal from the collection are published
    /// before best-effort cleanup. Existing handles report deletion and their
    /// state streams end; previously obtained state snapshots remain usable.
    /// Failed withdrawal leaves membership unchanged, and trash cleanup errors
    /// cannot invalidate deletion.
    ///
    /// The operation fails if the member does not exist, cannot be stopped,
    /// cancellation is requested, or its root cannot be moved into trash.
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
    /// and rollback. First yields the current list, then the latest list after
    /// each observed change. Slow consumers may miss intermediate states.
    ///
    /// Order is unspecified. The stream ends when all manager clones and
    /// operations retaining this collection are dropped, even if item handles
    /// remain alive.
    pub fn watch(&self) -> impl Stream<Item = Vec<T>> + Send + 'static + use<T> {
        let published = self.published.subscribe();
        let mut events = SelectAll::<Events<T>>::new();
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
                                let environment = handle.environment();
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
