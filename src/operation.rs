use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::Stream;
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use tokio_util::sync::CancellationToken;

use crate::error::Result;

/// Awaitable long-running work with operation-specific progress and cancellation.
#[must_use = "operations must be awaited or cancelled"]
pub struct Operation<T, P> {
    task: JoinHandle<Result<T>>,
    progress: watch::Receiver<Option<P>>,
    cancellation: CancellationToken,
}

impl<T, P> Operation<T, P> {
    pub(crate) fn spawn<F, Fut>(runtime: &Handle, work: F) -> Self
    where
        T: Send + 'static,
        P: Send + Sync + 'static,
        F: FnOnce(watch::Sender<Option<P>>, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let (progress, progress_rx) = watch::channel(None);
        let cancellation = CancellationToken::new();
        let task = runtime.spawn(work(progress, cancellation.clone()));

        Self {
            task,
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
        match Pin::new(&mut self.task).poll(context) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error.into())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T, P> Drop for Operation<T, P> {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[cfg(test)]
mod tests {
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
    async fn progress_starts_with_the_latest_value_and_is_independent() {
        let (start_tx, start_rx) = oneshot::channel();
        let (updated_tx, updated_rx) = oneshot::channel();
        let (later_tx, later_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel();
        let operation =
            Operation::spawn(&Handle::current(), |progress, _cancellation| async move {
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
        tokio::select! {
            biased;
            progress = first.next() => panic!("unexpected initial progress: {progress:?}"),
            _ = tokio::task::yield_now() => {}
        }

        start_tx.send(()).unwrap();
        updated_rx.await.unwrap();
        let mut second = Box::pin(operation.progress());

        assert_eq!(first.next().await, Some(Progress::Second));
        assert_eq!(second.next().await, Some(Progress::Second));

        later_tx.send(()).unwrap();
        assert_eq!(first.next().await, Some(Progress::Third));
        assert_eq!(second.next().await, Some(Progress::Third));

        finish_tx.send(()).unwrap();
        assert!(operation.await.is_ok());
        assert_eq!(first.next().await, None);
        assert_eq!(second.next().await, None);
    }

    #[tokio::test]
    async fn explicit_cancellation_waits_for_the_terminal_result() {
        let operation: Operation<(), Progress> =
            Operation::spawn(&Handle::current(), |_progress, cancellation| async move {
                cancellation.cancelled().await;
                Err(Error::Cancelled)
            });
        let mut progress = Box::pin(operation.progress());

        assert!(matches!(operation.cancel().await, Err(Error::Cancelled)));
        assert_eq!(progress.next().await, None);
    }

    #[tokio::test]
    async fn cancellation_after_commit_returns_the_result() {
        let (committed_tx, committed_rx) = oneshot::channel();
        let operation: Operation<_, Progress> =
            Operation::spawn(&Handle::current(), |_progress, _cancellation| async move {
                committed_tx.send(()).unwrap();
                Ok(42)
            });

        committed_rx.await.unwrap();
        assert_eq!(operation.cancel().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn dropping_requests_cancellation() {
        let (cancelled_tx, cancelled_rx) = oneshot::channel();
        let operation: Operation<(), Progress> =
            Operation::spawn(&Handle::current(), |_progress, cancellation| async move {
                cancellation.cancelled().await;
                cancelled_tx.send(()).unwrap();
                Err(Error::Cancelled)
            });

        drop(operation);
        cancelled_rx.await.unwrap();
    }
}
