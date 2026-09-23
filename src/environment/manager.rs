//! Shared environment collection lifecycle and observation.

#[cfg(feature = "fvs")]
use super::VirgoManager;
use super::{BackendSource, Environment, State};
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

type Members<T> = Arc<HashMap<Uuid, Arc<Environment<T>>>>;
pub(crate) struct Manager<T> {
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

impl<T: BackendSource + Send> Manager<T>
where
    State<T>: next_config::Config + Clone + PartialEq + Send + Sync,
{
    pub(crate) fn new(
        root: PathBuf,
        context: Context,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Arc<Self> {
        Arc::new(Self {
            root,
            context,
            #[cfg(feature = "fvs")]
            virgo,
            published: watch::channel(Arc::new(HashMap::new())).0,
        })
    }

    pub(crate) async fn load(
        root: PathBuf,
        context: Context,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Result<Arc<Self>> {
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
            let state: State<T> = match next_config::load(&file).await {
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
            members.insert(id, environment);
        }
        manager.published.send_replace(Arc::new(members));
        Ok(manager)
    }

    pub(crate) fn create(
        self: &Arc<Self>,
        data: T,
        runner: Addon<Component>,
        winebridge: Addon<Component>,
        umu: Option<Addon<Component>>,
    ) -> Operation<Arc<Environment<T>>> {
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
            manager.published.send_modify(|published| {
                Arc::make_mut(published).insert(id, environment.clone());
            });
            Ok(environment)
        })
    }

    pub(crate) fn list(&self) -> Vec<Arc<Environment<T>>> {
        self.published.borrow().values().cloned().collect()
    }

    pub(crate) fn open(&self, id: Uuid) -> Result<Arc<Environment<T>>> {
        self.published
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| EnvironmentError::NotFound(id).into())
    }

    pub(crate) fn delete(self: &Arc<Self>, id: Uuid) -> Operation<()> {
        let manager = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let environment = manager.open(id)?;
            environment
                .delete(&progress, &cancellation, || {
                    manager.published.send_modify(|published| {
                        Arc::make_mut(published).remove(&id);
                    });
                })
                .await
        })
    }

    pub(crate) fn watch(
        &self,
    ) -> impl Stream<Item = Vec<Arc<Environment<T>>>> + Send + 'static + use<T> {
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
                        for (id, environment) in members.iter() {
                            if subscribed.insert(*id) {
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
