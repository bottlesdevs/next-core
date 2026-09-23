//! Shared collection loading, membership and observation.

#[cfg(feature = "fvs")]
use super::VirgoManager;
use super::{BackendSource, Environment, State};
use crate::{Context, error::Result};
use futures_core::Stream;
use futures_util::{
    StreamExt,
    stream::{self, SelectAll},
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    pin::Pin,
    sync::Arc,
};
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;
use uuid::Uuid;

type Members<T> = Arc<HashMap<Uuid, Arc<Environment<T>>>>;
pub(crate) struct Registry<T>(watch::Sender<Members<T>>);
enum Event<T> {
    Membership(Members<T>),
    Changed,
}
type Events<T> = Pin<Box<dyn Stream<Item = Option<Event<T>>> + Send>>;

impl<T: BackendSource> Registry<T>
where
    State<T>: next_config::Config + Clone + PartialEq + Send + Sync,
{
    pub(crate) fn new() -> Self {
        Self(watch::channel(Arc::new(HashMap::new())).0)
    }

    pub(crate) async fn load(
        root: &Path,
        context: &Context,
        #[cfg(feature = "fvs")] virgo: &Arc<VirgoManager>,
    ) -> Result<Self> {
        let mut members = HashMap::new();
        let mut entries = match async_fs::read_dir(root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
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
                context.clone(),
                #[cfg(feature = "fvs")]
                virgo.clone(),
            )?;
            members.insert(id, environment);
        }
        Ok(Self(watch::channel(Arc::new(members)).0))
    }

    pub(crate) fn list(&self) -> Vec<Arc<Environment<T>>> {
        self.0.borrow().values().cloned().collect()
    }
    pub(crate) fn get(&self, id: Uuid) -> Option<Arc<Environment<T>>> {
        self.0.borrow().get(&id).cloned()
    }
    pub(crate) fn insert(&self, environment: Arc<Environment<T>>) -> Result<()> {
        let id = environment.state()?.id();
        self.0.send_modify(|published| {
            Arc::make_mut(published).insert(id, environment);
        });
        Ok(())
    }
    pub(crate) fn remove(&self, id: Uuid) {
        self.0.send_if_modified(|published| {
            let mut members = published.as_ref().clone();
            if members.remove(&id).is_none() {
                return false;
            }
            *published = Arc::new(members);
            true
        });
    }

    pub(crate) fn watch(
        &self,
    ) -> impl Stream<Item = Vec<Arc<Environment<T>>>> + Send + 'static + use<T> {
        let published = self.0.subscribe();
        let mut events = SelectAll::<Events<T>>::new();
        // End the aggregate when the registry closes, even if callers retain handles.
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
