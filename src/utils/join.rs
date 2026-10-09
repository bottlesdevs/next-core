use std::{collections::HashMap, future::ready, hash::Hash, sync::Arc};

use futures_core::Stream;
use futures_util::{
    FutureExt, StreamExt,
    stream::{self, BoxStream},
};
use tokio::sync::watch;
use tokio_stream::{StreamMap, wrappers::WatchStream};

enum Item<K, V, S> {
    Set(Arc<HashMap<K, V>>),
    Source(S),
}

/// Combines the latest item of every source in a published set.
///
/// Set changes resubscribe every source, so subscribing must be cheap and
/// yield the current item when first polled.
pub(crate) fn join<K, V, S>(
    set: watch::Receiver<Arc<HashMap<K, V>>>,
    subscribe: impl Fn(&V) -> BoxStream<'static, S> + Send + 'static,
) -> impl Stream<Item = Arc<HashMap<K, S>>> + Send + 'static
where
    K: Clone + Eq + Hash + Send + Sync + Unpin + 'static,
    V: Send + Sync + 'static,
    S: Clone + Send + 'static,
{
    let mut streams = StreamMap::<Option<K>, BoxStream<'static, Option<Item<K, V, S>>>>::new();
    // The sentinel ends the stream even if sources stay alive.
    streams.insert(
        None,
        WatchStream::new(set)
            .map(|set| Some(Item::Set(set)))
            .chain(stream::once(ready(None)))
            .boxed(),
    );
    stream::unfold(
        (streams, HashMap::new(), subscribe),
        |(mut streams, mut latest, subscribe)| async move {
            let (key, item) = streams.next().await?;
            match item? {
                Item::Set(set) => {
                    let keys = streams.keys().filter_map(Clone::clone).collect::<Vec<_>>();
                    for key in keys {
                        streams.remove(&Some(key));
                    }
                    latest.clear();
                    for (key, value) in set.iter() {
                        let mut source = subscribe(value);
                        if let Some(Some(item)) = source.next().now_or_never() {
                            latest.insert(key.clone(), item);
                        }
                        streams.insert(
                            Some(key.clone()),
                            source.map(|item| Some(Item::Source(item))).boxed(),
                        );
                    }
                }
                Item::Source(item) => {
                    latest.insert(key?, item);
                }
            }
            Some((Arc::new(latest.clone()), (streams, latest, subscribe)))
        },
    )
}
