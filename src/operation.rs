use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::Stream;
use tokio::sync::watch;
use tokio_stream::{StreamExt, wrappers::WatchStream};
use tokio_util::sync::CancellationToken;

use crate::error::Result;

/// Lazy Tokio-native work with operation-specific progress and cancellation.
///
/// The operation starts only when polled. Tokio clients can await it directly;
/// GPUI clients should poll it through `gpui_tokio::Tokio::spawn`.
#[must_use = "operations must be awaited or cancelled"]
pub struct Operation<T, P> {
    future: Pin<Box<dyn Future<Output = Result<T>> + Send + 'static>>,
    progress: watch::Receiver<Option<P>>,
    cancellation: CancellationToken,
}

impl<T, P> Operation<T, P> {
    pub(crate) fn new<F, Fut>(work: F) -> Self
    where
        T: Send + 'static,
        P: Send + Sync + 'static,
        F: FnOnce(watch::Sender<Option<P>>, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let (progress, progress_rx) = watch::channel(None);
        let cancellation = CancellationToken::new();
        let work_cancellation = cancellation.clone();
        let future = Box::pin(async move { work(progress, work_cancellation).await });

        Self {
            future,
            progress: progress_rx,
            cancellation,
        }
    }
}

impl<T, P> Operation<T, P>
where
    P: Clone + Send + Sync + 'static,
{
    /// Subscribe to the latest emitted progress and later changes.
    pub fn progress(&self) -> impl Stream<Item = P> + Send + 'static {
        WatchStream::new(self.progress.clone()).filter_map(|progress| progress)
    }

    /// Request cancellation and wait for cancellation or committed completion.
    pub async fn cancel(mut self) -> Result<T> {
        self.cancellation.cancel();
        (&mut self).await
    }
}

impl<T, P> Future for Operation<T, P> {
    type Output = Result<T>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(context)
    }
}

impl<T, P> Drop for Operation<T, P> {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use crate::error::Error;
    use tokio::sync::oneshot;
    use tokio_stream::StreamExt;

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Progress {
        First,
        Second,
        Third,
    }

    #[tokio::test]
    async fn operation_is_lazy() {
        let started = Arc::new(AtomicBool::new(false));
        let work_started = started.clone();
        let operation: Operation<(), Progress> = Operation::new(move |_, _| {
            work_started.store(true, Ordering::Relaxed);
            async { Ok(()) }
        });

        tokio::task::yield_now().await;
        assert!(!started.load(Ordering::Relaxed));
        operation.await.unwrap();
        assert!(started.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn progress_starts_with_the_latest_value_and_is_independent() {
        let (start_tx, start_rx) = oneshot::channel();
        let (updated_tx, updated_rx) = oneshot::channel();
        let (later_tx, later_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel();
        let operation = Operation::new(|progress, _cancellation| async move {
            start_rx.await.unwrap();
            progress.send_replace(Some(Progress::First));
            progress.send_replace(Some(Progress::Second));
            updated_tx.send(()).unwrap();
            later_rx.await.unwrap();
            progress.send_replace(Some(Progress::Third));
            finish_rx.await.unwrap();
            Ok(())
        });

        let mut first = Box::pin(operation.progress());
        let late_progress = operation.progress.clone();
        tokio::select! {
            biased;
            progress = first.next() => panic!("unexpected initial progress: {progress:?}"),
            _ = tokio::task::yield_now() => {}
        }

        let task = tokio::spawn(operation);
        start_tx.send(()).unwrap();
        updated_rx.await.unwrap();
        let mut second = Box::pin(WatchStream::new(late_progress).filter_map(|progress| progress));

        assert_eq!(first.next().await, Some(Progress::Second));
        assert_eq!(second.next().await, Some(Progress::Second));

        later_tx.send(()).unwrap();
        assert_eq!(first.next().await, Some(Progress::Third));
        assert_eq!(second.next().await, Some(Progress::Third));

        finish_tx.send(()).unwrap();
        assert!(task.await.unwrap().is_ok());
        assert_eq!(first.next().await, None);
        assert_eq!(second.next().await, None);
    }

    #[tokio::test]
    async fn explicit_cancellation_waits_for_the_terminal_result() {
        let operation: Operation<(), Progress> =
            Operation::new(|_progress, cancellation| async move {
                cancellation.cancelled().await;
                Err(Error::Cancelled)
            });
        let mut progress = Box::pin(operation.progress());

        assert!(matches!(operation.cancel().await, Err(Error::Cancelled)));
        assert_eq!(progress.next().await, None);
    }

    #[tokio::test]
    async fn cancellation_after_commit_returns_the_result() {
        let (committed_tx, mut committed_rx) = oneshot::channel();
        let mut operation: Operation<_, Progress> =
            Operation::new(|_progress, _cancellation| async move {
                committed_tx.send(()).unwrap();
                tokio::task::yield_now().await;
                Ok(42)
            });

        tokio::select! {
            biased;
            result = &mut committed_rx => result.unwrap(),
            result = &mut operation => panic!("operation completed before commit was observed: {result:?}"),
        }
        assert_eq!(operation.cancel().await.unwrap(), 42);
    }

    #[test]
    fn dropping_requests_cancellation() {
        let operation: Operation<(), Progress> = Operation::new(|_, _| async { Ok(()) });
        let cancellation = operation.cancellation.clone();

        drop(operation);
        assert!(cancellation.is_cancelled());
    }
}
