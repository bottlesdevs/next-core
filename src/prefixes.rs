//! Live collections of persisted prefixes.
//!
//! [`Prefixes`] is loaded once by [`crate::Bottles`] and then updated by create,
//! edit, rollback, and delete operations. It does not poll the filesystem for
//! changes made outside this crate.

use crate::prefix::{Inner, Inventory};
use crate::{
    Addon, Context, Operation, Prefix, PrefixBackend, PrefixError, PrefixState, Progress, Runner,
    Stage, Umu, WineBridge, error::Result,
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

type Members = Arc<HashMap<Uuid, Prefix>>;

enum Event {
    Membership(Members),
    Changed,
}
type Events = Pin<Box<dyn Stream<Item = Option<Event>> + Send>>;

/// Tracks the live collection of prefixes.
///
/// Obtain it from [`crate::Bottles::prefixes`]. Clones share collection
/// membership. Opening the same UUID through clones
/// returns handles to the same live state. The collection is loaded once from
/// library-managed storage and updated by its operations; it does not
/// observe external filesystem changes.
#[derive(Clone)]
pub struct Prefixes {
    root: PathBuf,
    context: Context,
    members: watch::Sender<Members>,
}

impl Prefixes {
    /// Returns cloned handles for the currently known members.
    ///
    /// Order is unspecified and must not be used as an identity or stable
    /// presentation order.
    pub fn list(&self) -> Vec<Prefix> {
        self.members.borrow().values().cloned().collect()
    }

    /// Looks up a member without filesystem or runtime work.
    ///
    /// Repeated calls through this collection or its clones return handles to the
    /// same live state. Only members loaded at startup or created through this
    /// collection are opened.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::NotFound`] if `id` is not in the collection.
    pub fn open(&self, id: Uuid) -> Result<Prefix> {
        self.members
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| PrefixError::NotFound(id).into())
    }

    /// Returns the current state of every prefix, ordered by identifier.
    pub fn inventory(&self) -> Inventory {
        inventory(&self.members.borrow())
    }

    /// Observes the inventory, including creation, edits, rollback, and deletion.
    ///
    /// First yields the current inventory, then the latest one after each
    /// observed change. Slow consumers may miss intermediate inventories. The
    /// stream ends when all collection clones and operations retaining this
    /// collection are dropped, even if prefix handles remain alive.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Prefixes;
    /// # use futures_lite::StreamExt;
    /// # async fn observe(prefixes: &Prefixes) {
    /// let updates = prefixes.watch();
    /// futures_lite::pin!(updates);
    /// if let Some(inventory) = updates.next().await {
    ///     println!("{} prefixes", inventory.len());
    /// }
    /// # }
    /// ```
    pub fn watch(&self) -> impl Stream<Item = Inventory> + Send + 'static + use<> {
        let members = self.members.subscribe();
        let mut events = SelectAll::<Events>::new();
        // End the aggregate when the collection closes, even if callers retain handles.
        events.push(Box::pin(
            WatchStream::new(members.clone())
                .map(|members| Some(Event::Membership(members)))
                .chain(stream::once(async { None })),
        ));
        stream::unfold(
            (members, events, HashSet::new()),
            |(members, mut events, mut subscribed)| async move {
                match events.next().await?? {
                    Event::Membership(current) => {
                        subscribed.retain(|id| current.contains_key(id));
                        for (id, prefix) in current.iter() {
                            if subscribed.insert(*id) {
                                events.push(Box::pin(
                                    WatchStream::from_changes(prefix.0.state.subscribe())
                                        .take_while(|state| std::future::ready(state.is_some()))
                                        .map(|_| Some(Event::Changed)),
                                ));
                            }
                        }
                    }
                    Event::Changed => {}
                }
                let inventory = inventory(&members.borrow());
                Some((inventory, (members, events, subscribed)))
            },
        )
    }

    /// Loads valid UUID-named prefixes found below `root`.
    ///
    /// A missing root produces an empty collection. Entries without `state.toml`
    /// and non-directory entries are ignored; malformed or invalid persisted
    /// state aborts the entire load.
    ///
    /// # Errors
    ///
    /// Returns an error when directory enumeration, state loading, validation,
    /// or prefix reconstruction fails.
    pub(crate) async fn load(root: PathBuf, context: Context) -> Result<Self> {
        let prefixes = Self {
            root,
            context,
            members: watch::channel(Arc::default()).0,
        };
        let mut members = HashMap::new();
        let mut entries = match async_fs::read_dir(&prefixes.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(prefixes),
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next().await {
            let root = entry?.path();
            let file = root.join("state.toml");
            let state: PrefixState = match next_config::load(&file).await {
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
            let prefix = Prefix(Inner::new(state, root, prefixes.context.clone())?);
            members.insert(prefix.id(), prefix);
        }
        prefixes.members.send_replace(Arc::new(members));
        Ok(prefixes)
    }

    /// Creates an operation that initializes and persists a prefix.
    ///
    /// A new UUID is assigned when the operation starts. Addon coexistence
    /// requirements are checked before creating files; missing requirements fail
    /// without selecting or downloading other addons. Standard creation
    /// initializes Wine without FVS. Virgo, available with the `fvs` feature,
    /// builds missing shared layers before composing private storage.
    /// The prefix is added to this collection only after initialization and
    /// `state.toml` persistence succeed. Cancellation or persistence failure
    /// after backend initialization attempts best-effort removal; earlier
    /// backend failures, dropping a started operation, or cleanup failure can
    /// leave an unregistered directory that a later core startup may discover.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::RequiresAddon`] for missing coexistence
    /// requirements before creating any files. Awaiting the operation can also
    /// fail during cancellation, service startup, I/O, prefix initialization, or
    /// state persistence.
    pub fn create(
        &self,
        backend: PrefixBackend,
        runner: Addon<Runner>,
        winebridge: Addon<WineBridge>,
        umu: Option<Addon<Umu>>,
    ) -> Operation<Prefix> {
        let prefixes = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let state = PrefixState::new(backend, runner, winebridge, umu);
            state.validate()?;
            let root = prefixes.root.join(Uuid::new_v4().to_string());
            let prefix = Prefix(Inner::new(state, root, prefixes.context.clone())?);
            prefix.0.create(&progress, &cancellation).await?;
            prefixes.members.send_modify(|members| {
                Arc::make_mut(members).insert(prefix.id(), prefix.clone());
            });
            Ok(prefix)
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
        let prefixes = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let handle = prefixes.open(id)?;
            handle
                .0
                .delete(&progress, &cancellation, || {
                    prefixes.members.send_modify(|members| {
                        Arc::make_mut(members).remove(&id);
                    });
                })
                .await
        })
    }
}

/// Collects the published state of every member, ordered by identifier.
fn inventory(members: &Members) -> Inventory {
    Arc::new(
        members
            .iter()
            .filter_map(|(id, prefix)| Some((*id, prefix.state().ok()?)))
            .collect(),
    )
}
