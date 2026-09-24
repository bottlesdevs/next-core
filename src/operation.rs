//! Lazy asynchronous work with progress reporting and cooperative cancellation.
//!
//! Core mutations return [`Operation`] instead of spawning tasks. The caller
//! chooses the executor and controls when work starts by polling the operation.
//! Progress observation does not drive the work.

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

/// Counts completed work within an operation stage.
///
/// The unit depends on the stage: downloads use bytes, while backend services
/// may report their own units. `total` is absent when the amount of work is not
/// known in advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transfer {
    /// Work reported complete in the stage's unit.
    pub current: u64,
    /// Expected work in the same unit, when known.
    pub total: Option<u64>,
}

/// Describes the latest progress emitted by an [`Operation`].
///
/// Progress is advisory. Operations may omit stages, and slow consumers may
/// miss intermediate updates. Await the operation itself to determine when it
/// has finished and whether it succeeded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Progress {
    /// The work currently being performed.
    pub stage: Stage,
    /// Quantified progress for the stage, when available.
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

    /// Returns the completed fraction as a value from `0.0` through `1.0`.
    ///
    /// Returns `None` when no transfer is reported, its total is unknown, or
    /// its total is zero. Values beyond the reported total are clamped to
    /// `1.0`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::{Progress, Stage, Transfer};
    ///
    /// let progress = Progress {
    ///     stage: Stage::Extracting,
    ///     transfer: Some(Transfer { current: 6, total: Some(3) }),
    /// };
    /// assert_eq!(progress.fraction(), Some(1.0));
    /// ```
    pub fn fraction(&self) -> Option<f32> {
        let transfer = self.transfer?;
        let total = transfer.total.filter(|total| *total > 0)?;
        Some((transfer.current as f64 / total as f64).clamp(0.0, 1.0) as f32)
    }
}

/// Identifies a phase of work reported by an [`Operation`].
///
/// An operation is not required to emit every phase or follow the order in
/// which the variants are declared.
///
/// # Examples
///
/// ```
/// use bottles_core::Stage;
///
/// assert_eq!(Stage::Preparing.to_string(), "Preparing");
/// assert_eq!(
///     Stage::Downloading { file: "runner".into() }.to_string(),
///     "Downloading runner",
/// );
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Stage {
    /// Resolves inputs and validates prerequisites.
    Preparing,
    /// Stops an active environment before mutation.
    Stopping,
    /// Downloads a named artifact.
    Downloading {
        /// The file name or caller-facing label of the current download.
        file: String,
    },
    /// Verifies a named artifact.
    Verifying {
        /// The file name or caller-facing label of the item being verified.
        file: String,
    },
    /// Extracts an archive.
    Extracting,
    /// Initializes a Wine prefix.
    CreatingPrefix,
    #[cfg(feature = "fvs")]
    /// Records an FVS revision before a change.
    Checkpointing,
    #[cfg(feature = "fvs")]
    /// Restores an FVS revision.
    Restoring,
    /// Reconstructs environment state after a selection changes.
    Rebuilding,
    /// Applies configuration to an environment.
    Configuring,
    /// Removes installed data.
    Removing,
    #[cfg(feature = "fvs")]
    /// Publishes an immutable FVS layer.
    Committing,
}

impl fmt::Display for Stage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preparing => formatter.write_str("Preparing"),
            Self::Stopping => formatter.write_str("Stopping"),
            Self::Downloading { file } => write!(formatter, "Downloading {file}"),
            Self::Verifying { file } => write!(formatter, "Verifying {file}"),
            Self::Extracting => formatter.write_str("Extracting"),
            Self::CreatingPrefix => formatter.write_str("Creating prefix"),
            #[cfg(feature = "fvs")]
            Self::Checkpointing => formatter.write_str("Checkpointing"),
            #[cfg(feature = "fvs")]
            Self::Restoring => formatter.write_str("Restoring"),
            Self::Rebuilding => formatter.write_str("Rebuilding"),
            Self::Configuring => formatter.write_str("Configuring"),
            Self::Removing => formatter.write_str("Removing"),
            #[cfg(feature = "fvs")]
            Self::Committing => formatter.write_str("Committing"),
        }
    }
}

/// A lazy future with progress reporting and cooperative cancellation.
///
/// Creating an operation does not start it. Work begins on its first poll;
/// the caller owns execution and must keep the future driven to completion.
///
/// Dropping an operation abandons its future without requesting cancellation,
/// so cleanup encoded in that future will not run. External work already
/// accepted by another process or plugin may still finish independently. Use
/// [`cancel`](Self::cancel) and await the result when the operation must reach a
/// defined terminal state.
#[must_use = "operations must be awaited, cancelled, or spawned by an executor"]
pub struct Operation<T> {
    future: Pin<Box<dyn Future<Output = Result<T>> + Send + 'static>>,
    progress: watch::Receiver<Option<Progress>>,
    cancellation: CancellationToken,
}

impl<T> Operation<T> {
    /// Creates an operation whose work closure is invoked on its first poll.
    ///
    /// If cancellation is requested before that poll, the closure is not invoked
    /// and the operation returns [`Error::Cancelled`](crate::error::Error::Cancelled).
    ///
    /// The work must treat cancellation as cooperative and keep any required
    /// asynchronous cleanup in the returned future so [`Operation::cancel`] can
    /// drive it. Progress senders must not outlive that future so progress streams
    /// close when the operation terminates.
    pub(crate) fn new<F, Fut>(work: F) -> Self
    where
        T: Send + 'static,
        F: FnOnce(watch::Sender<Option<Progress>>, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let (progress, progress_rx) = watch::channel(None);
        let cancellation = CancellationToken::new();
        let work_cancellation = cancellation.clone();
        let future = Box::pin(async move {
            if work_cancellation.is_cancelled() {
                Err(crate::error::Error::Cancelled)
            } else {
                work(progress, work_cancellation).await
            }
        });

        Self {
            future,
            progress: progress_rx,
            cancellation,
        }
    }

    /// Maps a successful result while preserving progress and cancellation.
    ///
    /// Errors pass through unchanged and do not invoke `map`.
    pub fn map<U, F>(self, map: F) -> Operation<U>
    where
        T: Send + 'static,
        U: Send + 'static,
        F: FnOnce(T) -> U + Send + 'static,
    {
        let Self {
            future,
            progress,
            cancellation,
        } = self;
        Operation {
            future: Box::pin(async move { future.await.map(map) }),
            progress,
            cancellation,
        }
    }

    /// Returns a token that can request cancellation without consuming the operation.
    ///
    /// Cancelling the token does not poll the operation or wait for cleanup. If
    /// cancellation precedes the first poll, the work closure is skipped;
    /// otherwise the running operation observes the token only at its documented
    /// cancellation points. Await the operation to obtain its terminal result.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::Operation;
    ///
    /// # async fn example(operation: Operation<()>) -> Result<(), bottles_core::error::Error> {
    /// let cancellation = operation.cancellation_token();
    /// cancellation.cancel();
    /// let _ = operation.await;
    /// # Ok(())
    /// # }
    /// ```
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

impl<T> Operation<T> {
    /// Subscribes to the latest emitted progress and subsequent changes.
    ///
    /// If progress has already been emitted, the stream yields the latest value
    /// first. Slow consumers may see updates coalesced. The stream does not drive
    /// or retain the operation and ends when the operation's progress sender is
    /// dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::Operation;
    /// use futures_lite::StreamExt;
    ///
    /// # async fn example(operation: Operation<()>) {
    /// let mut progress = operation.progress();
    /// while let Some(update) = progress.next().await {
    ///     println!("{}", update.stage);
    /// }
    /// # }
    /// ```
    pub fn progress(&self) -> impl Stream<Item = Progress> + Send + 'static {
        WatchStream::new(self.progress.clone()).filter_map(|progress| progress)
    }

    /// Requests cancellation and drives the operation to its terminal result.
    ///
    /// An unpolled operation is cancelled without invoking its work closure.
    /// Once work has started, cancellation is cooperative and may only be
    /// observed at operation-specific checkpoints. If the work has passed its
    /// cancellation boundary, this may return its successful result or another
    /// error instead of [`Error::Cancelled`](crate::error::Error::Cancelled).
    /// This method neither aborts the future nor imposes a timeout, so work that
    /// ignores cancellation can keep it pending indefinitely.
    ///
    /// # Errors
    ///
    /// Returns the operation's terminal error. Cooperative work commonly returns
    /// [`Error::Cancelled`](crate::error::Error::Cancelled), but work that has
    /// crossed its cancellation boundary may return another result.
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
            let mut operation: Operation<()> =
                Operation::new(|_progress, cancellation| async move {
                    cancellation.cancelled().await;
                    Err(Error::Cancelled)
                });
            let mut progress = Box::pin(operation.progress());

            assert!(
                futures_lite::future::poll_once(&mut operation)
                    .await
                    .is_none()
            );
            assert!(matches!(operation.cancel().await, Err(Error::Cancelled)));
            assert_eq!(progress.next().await, None);
        });
    }

    #[test]
    fn cancellation_before_first_poll_does_not_invoke_work() {
        futures_lite::future::block_on(async {
            let invoked = Arc::new(AtomicBool::new(false));
            let work_invoked = invoked.clone();
            let operation: Operation<()> = Operation::new(move |_, _| {
                work_invoked.store(true, Ordering::Relaxed);
                async { Ok(()) }
            });
            operation.cancellation_token().cancel();

            assert!(matches!(operation.await, Err(Error::Cancelled)));
            assert!(!invoked.load(Ordering::Relaxed));
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
    fn dropping_abandons_without_requesting_cancellation() {
        futures_lite::future::block_on(async {
            let mut operation: Operation<()> =
                Operation::new(|_, _| async { std::future::pending::<Result<()>>().await });
            let cancellation = operation.cancellation_token();

            assert!(
                futures_lite::future::poll_once(&mut operation)
                    .await
                    .is_none()
            );
            drop(operation);

            assert!(!cancellation.is_cancelled());
        });
    }

    #[test]
    fn detached_executor_task_runs_to_completion() {
        let executor = async_executor::Executor::new();
        futures_lite::future::block_on(executor.run(async {
            let (finished_tx, finished_rx) = oneshot::channel();
            let operation: Operation<()> = Operation::new(|_, _| async move {
                finished_tx.send(()).unwrap();
                Ok(())
            });

            executor.spawn(operation).detach();

            finished_rx.await.unwrap();
        }));
    }
}
