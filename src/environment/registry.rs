//! Shared collection loading, identity interning and observation.

#[cfg(feature = "fvs")]
use super::VirgoManager;
use super::{Environment, EnvironmentOwnerState};
use crate::{Addons, Context, error::Result};
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

impl<T: EnvironmentOwnerState> Registry<T> {
    pub(crate) fn new() -> Self {
        Self(watch::channel(Arc::new(HashMap::new())).0)
    }

    pub(crate) async fn load(
        root: &Path,
        context: &Context,
        addons: &Addons,
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
            let file = root.join(T::FILE_NAME);
            if !async_fs::metadata(&file)
                .await
                .is_ok_and(|entry| entry.is_file())
            {
                continue;
            }
            let loaded = async {
                let state: T = next_config::load(&file).await?;
                let id = state.id();
                let environment = Environment::from_state(
                    state,
                    root,
                    context.clone(),
                    addons.clone(),
                    #[cfg(feature = "fvs")]
                    virgo.clone(),
                )?;
                Ok::<_, crate::error::Error>((id, environment))
            }
            .await;
            match loaded {
                Ok((id, environment)) => {
                    members.insert(id, environment);
                }
                Err(error) => {
                    tracing::warn!(path = %file.display(), "skipping incompatible or unreadable environment: {error}")
                }
            }
        }
        Ok(Self(watch::channel(Arc::new(members)).0))
    }

    pub(crate) fn list(&self) -> Vec<Arc<Environment<T>>> {
        self.0.borrow().values().cloned().collect()
    }
    pub(crate) fn get(&self, id: Uuid) -> Option<Arc<Environment<T>>> {
        self.0.borrow().get(&id).cloned()
    }
    pub(crate) fn intern(&self, environment: Arc<Environment<T>>) -> Result<Arc<Environment<T>>> {
        let id = environment.state()?.id();
        let mut interned = environment.clone();
        self.0.send_if_modified(|published| {
            if let Some(current) = published.get(&id) {
                interned = current.clone();
                return false;
            }
            let mut members = published.as_ref().clone();
            members.insert(id, environment);
            *published = Arc::new(members);
            true
        });
        Ok(interned)
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
