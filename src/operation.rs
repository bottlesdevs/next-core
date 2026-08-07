use std::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::Stream;
use tokio::sync::watch;
use tokio_stream::{StreamExt, wrappers::WatchStream};
use tokio_util::sync::CancellationToken;

use crate::error::Result;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transfer {
    pub current: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Progress {
    pub stage: Stage,
    pub transfer: Option<Transfer>,
}

impl Progress {
    pub(crate) fn new(stage: Stage) -> Self {
        Self {
            stage,
            transfer: None,
        }
    }

    pub(crate) fn transferring(stage: Stage, transfer: Transfer) -> Self {
        Self {
            stage,
            transfer: Some(transfer),
        }
    }

    pub fn fraction(&self) -> Option<f32> {
        let transfer = self.transfer?;
        let total = transfer.total.filter(|total| *total > 0)?;
        Some((transfer.current as f64 / total as f64).clamp(0.0, 1.0) as f32)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Stage {
    Preparing,
    Stopping,
    Downloading {
        file: String,
        index: usize,
        total: usize,
    },
    Verifying {
        file: String,
    },
    Extracting,
    CreatingPrefix,
    Checkpointing,
    Restoring,
    Rebuilding,
    Configuring,
    Removing,
    Committing,
}

impl fmt::Display for Stage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preparing => formatter.write_str("Preparing"),
            Self::Stopping => formatter.write_str("Stopping"),
            Self::Downloading { file, index, total } => {
                write!(formatter, "Downloading {file} ({index}/{total})")
            }
            Self::Verifying { file } => write!(formatter, "Verifying {file}"),
            Self::Extracting => formatter.write_str("Extracting"),
            Self::CreatingPrefix => formatter.write_str("Creating prefix"),
            Self::Checkpointing => formatter.write_str("Checkpointing"),
            Self::Restoring => formatter.write_str("Restoring"),
            Self::Rebuilding => formatter.write_str("Rebuilding"),
            Self::Configuring => formatter.write_str("Configuring"),
            Self::Removing => formatter.write_str("Removing"),
            Self::Committing => formatter.write_str("Committing"),
        }
    }
}

/// Lazy runtime-independent work with operation-specific progress and cancellation.
///
/// The operation starts only when polled. Async clients can await it directly;
/// GPUI clients should poll it through `gpui_tokio::Tokio::spawn`.
#[must_use = "operations must be awaited or cancelled"]
pub struct Operation<T> {
    future: Pin<Box<dyn Future<Output = Result<T>> + Send + 'static>>,
    progress: watch::Receiver<Option<Progress>>,
    cancellation: CancellationToken,
}

impl<T> Operation<T> {
    pub(crate) fn new<F, Fut>(work: F) -> Self
    where
        T: Send + 'static,
        F: FnOnce(watch::Sender<Option<Progress>>, CancellationToken) -> Fut + Send + 'static,
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

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

impl<T> Operation<T> {
    /// Subscribe to the latest emitted progress and later changes.
    pub fn progress(&self) -> impl Stream<Item = Progress> + Send + 'static {
        WatchStream::new(self.progress.clone()).filter_map(|progress| progress)
    }

    /// Request cancellation and wait for cancellation or committed completion.
    pub async fn cancel(mut self) -> Result<T> {
        self.cancellation.cancel();
        (&mut self).await
    }
}

impl<T> Future for Operation<T> {
    type Output = Result<T>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(context)
    }
}

impl<T> Drop for Operation<T> {
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

    use super::*;
    use crate::error::Error;
    use tokio::sync::oneshot;
    use tokio_stream::StreamExt;

    fn update(stage: Stage) -> Progress {
        Progress::new(stage)
    }

    #[test]
    fn progress_fraction_is_bounded() {
        assert_eq!(Progress::new(Stage::Preparing).fraction(), None);
        assert_eq!(
            Progress::transferring(
                Stage::Downloading {
                    file: "runner".into(),
                    index: 1,
                    total: 1,
                },
                Transfer {
                    current: 150,
                    total: Some(100),
                },
            )
            .fraction(),
            Some(1.0)
        );
    }

    #[test]
    fn operation_is_lazy() {
        let started = Arc::new(AtomicBool::new(false));
        let work_started = started.clone();
        let operation: Operation<()> = Operation::new(move |_, _| {
            work_started.store(true, Ordering::Relaxed);
            async { Ok(()) }
        });

        assert!(!started.load(Ordering::Relaxed));
        futures_lite::future::block_on(operation).unwrap();
        assert!(started.load(Ordering::Relaxed));
    }

    #[test]
    fn progress_starts_with_the_latest_value_and_is_independent() {
        futures_lite::future::block_on(async {
            let (start_tx, start_rx) = oneshot::channel();
            let (updated_tx, updated_rx) = oneshot::channel();
            let (later_tx, later_rx) = oneshot::channel();
            let (finish_tx, finish_rx) = oneshot::channel();
            let operation = Operation::new(|progress, _cancellation| async move {
                start_rx.await.unwrap();
                progress.send_replace(Some(update(Stage::Preparing)));
                progress.send_replace(Some(update(Stage::Stopping)));
                updated_tx.send(()).unwrap();
                later_rx.await.unwrap();
                progress.send_replace(Some(update(Stage::Removing)));
                finish_rx.await.unwrap();
                Ok(())
            });

            let mut first = Box::pin(operation.progress());
            let late_progress = operation.progress.clone();
            assert!(
                futures_lite::future::poll_once(first.next())
                    .await
                    .is_none()
            );

            let observe = async move {
                start_tx.send(()).unwrap();
                updated_rx.await.unwrap();
                let mut second =
                    Box::pin(WatchStream::new(late_progress).filter_map(|progress| progress));

                assert_eq!(first.next().await, Some(update(Stage::Stopping)));
                assert_eq!(second.next().await, Some(update(Stage::Stopping)));

                later_tx.send(()).unwrap();
                assert_eq!(first.next().await, Some(update(Stage::Removing)));
                assert_eq!(second.next().await, Some(update(Stage::Removing)));

                finish_tx.send(()).unwrap();
                assert_eq!(first.next().await, None);
                assert_eq!(second.next().await, None);
            };
            let (result, ()) = futures_util::future::join(operation, observe).await;
            assert!(result.is_ok());
        });
    }

    #[test]
    fn explicit_cancellation_waits_for_the_terminal_result() {
        futures_lite::future::block_on(async {
            let operation: Operation<()> = Operation::new(|_progress, cancellation| async move {
                cancellation.cancelled().await;
                Err(Error::Cancelled)
            });
            let mut progress = Box::pin(operation.progress());

            assert!(matches!(operation.cancel().await, Err(Error::Cancelled)));
            assert_eq!(progress.next().await, None);
        });
    }

    #[test]
    fn cancellation_after_commit_returns_the_result() {
        futures_lite::future::block_on(async {
            let (committed_tx, committed_rx) = oneshot::channel();
            let mut operation: Operation<_> =
                Operation::new(|_progress, _cancellation| async move {
                    committed_tx.send(()).unwrap();
                    futures_lite::future::yield_now().await;
                    Ok(42)
                });

            assert!(
                futures_lite::future::poll_once(&mut operation)
                    .await
                    .is_none()
            );
            committed_rx.await.unwrap();
            assert_eq!(operation.cancel().await.unwrap(), 42);
        });
    }

    #[test]
    fn dropping_requests_cancellation() {
        let operation: Operation<()> = Operation::new(|_, _| async { Ok(()) });
        let cancellation = operation.cancellation.clone();

        drop(operation);
        assert!(cancellation.is_cancelled());
    }
}
