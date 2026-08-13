//! Prefix storage backends and checkpointed addon mutation.
//!
//! [`Prefix`] is persisted as part of each bottle's state. Standard storage
//! mutates a conventional prefix directly; Virgo stores an ordered FVS layer
//! stack with a per-bottle writable upper directory. With the default `fvs`
//! feature, addon installation and removal use an FVS rollback checkpoint.

mod standard;
#[cfg(feature = "fvs")]
mod virgo;

use std::{future::Future, path::Path};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
#[cfg(feature = "fvs")]
use {
    crate::{Stage, Transfer, error::Error},
    futures_core::Stream,
    futures_util::TryStreamExt,
    fvs_rs::{
        Commit, Layer, Progress as FvsProgress, Repository, RestoreResponse,
        error::Error as FvsError,
    },
};

use crate::{Context, Progress, bottle::Storage, error::Result, runner::Runner};

/// Identifies rollback checkpoints that must not appear as user snapshots.
///
/// Snapshot filtering compares this persisted value exactly, so changing it
/// would expose checkpoints created by older versions.
#[cfg(feature = "fvs")]
pub(crate) const AUTO_CHECKPOINT_MESSAGE: &str = "bottles-next:auto-checkpoint";
#[cfg(feature = "fvs")]
pub(crate) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;

/// Backend-specific state persisted in [`crate::bottle::BottleState`].
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub(crate) enum Prefix {
    Standard,
    #[cfg(feature = "fvs")]
    Virgo {
        /// Mount order: shared base, runner adapter, then installed addon layers.
        #[serde(default)]
        layers: Vec<Layer>,
    },
}

#[cfg(feature = "fvs")]
impl From<&FvsProgress> for Transfer {
    fn from(progress: &FvsProgress) -> Self {
        // FVS uses negative counters when progress is unavailable. Core progress
        // uses unsigned values and represents an unavailable total explicitly.
        Self {
            current: progress.current.try_into().unwrap_or_default(),
            total: progress.total.try_into().ok().filter(|total| *total > 0),
        }
    }
}

impl Prefix {
    pub(crate) async fn create(
        storage: Storage,
        bottle_path: &Path,
        runner: &dyn Runner,
        runner_key: &str,
        context: &Context,
    ) -> Result<Self> {
        #[cfg(not(feature = "fvs"))]
        let _ = (runner_key, context);
        match storage {
            Storage::Standard => {
                standard::create(bottle_path, runner).await?;
                Ok(Self::Standard)
            }
            #[cfg(feature = "fvs")]
            Storage::Virgo => Ok(Self::Virgo {
                layers: virgo::create(bottle_path, runner, runner_key, context).await?,
            }),
        }
    }

    pub(crate) fn kind(&self) -> Storage {
        match self {
            Self::Standard => Storage::Standard,
            #[cfg(feature = "fvs")]
            Self::Virgo { .. } => Storage::Virgo,
        }
    }

    pub(crate) async fn prepare(&self, bottle_path: &Path, context: &Context) -> Result<()> {
        let _ = (bottle_path, context);
        match self {
            Self::Standard => Ok(()),
            #[cfg(feature = "fvs")]
            Self::Virgo { layers } => virgo::prepare(bottle_path, layers, context).await,
        }
    }

    pub(crate) async fn stop(&self, bottle_path: &Path, context: &Context) -> Result<()> {
        let _ = (bottle_path, context);
        match self {
            Self::Standard => Ok(()),
            #[cfg(feature = "fvs")]
            Self::Virgo { .. } => virgo::stop(bottle_path, context).await,
        }
    }

    pub(crate) async fn rebuild(
        &mut self,
        runner: &dyn Runner,
        runner_key: &str,
        installed: &[Uuid],
        context: &Context,
    ) -> Result<()> {
        match self {
            Self::Standard => {
                let _ = (runner, runner_key, installed, context);
                Ok(())
            }
            #[cfg(feature = "fvs")]
            Self::Virgo { layers } => {
                // Resolve the complete replacement before changing persisted state. A
                // missing cached addon therefore leaves the old layer stack intact.
                virgo::rebuild(layers, runner, runner_key, installed, context).await
            }
        }
    }

    pub(crate) async fn install<F, P>(
        &mut self,
        bottle_path: &Path,
        item_id: Uuid,
        replaced_id: Option<Uuid>,
        execute: F,
        context: &Context,
        cancellation: &CancellationToken,
        on_progress: P,
    ) -> Result<()>
    where
        F: for<'a> std::ops::AsyncFnOnce(&'a Path) -> Result<()>,
        P: FnMut(Progress),
    {
        let _ = (item_id, replaced_id);
        let work = async {
            match self {
                Self::Standard => standard::install(bottle_path, execute).await,
                #[cfg(feature = "fvs")]
                Self::Virgo { layers } => {
                    virgo::install(bottle_path, layers, item_id, replaced_id, execute, context)
                        .await
                }
            }
        };
        transact(bottle_path, context, work, cancellation, on_progress).await
    }

    pub(crate) async fn uninstall<F, P>(
        &mut self,
        bottle_path: &Path,
        item_id: Uuid,
        execute: F,
        context: &Context,
        cancellation: &CancellationToken,
        on_progress: P,
    ) -> Result<()>
    where
        F: for<'a> std::ops::AsyncFnOnce(&'a Path, bool) -> Result<()>,
        P: FnMut(Progress),
    {
        let _ = item_id;
        let work = async {
            match self {
                Self::Standard => standard::uninstall(bottle_path, execute).await,
                #[cfg(feature = "fvs")]
                Self::Virgo { layers } => {
                    virgo::uninstall(bottle_path, layers, item_id, execute, context).await
                }
            }
        };
        transact(bottle_path, context, work, cancellation, on_progress).await
    }
}

/// Runs a prefix mutation behind a rollback checkpoint.
///
/// Cancellation is checked after checkpointing and after successful work. Any
/// work error or observed cancellation triggers a restore. If restore also
/// fails, the restore failure is logged and the original error is preserved.
/// Dropping the surrounding [`crate::Operation`] abandons this future and does
/// not drive the restore path.
#[cfg(feature = "fvs")]
async fn transact<F, T, P>(
    bottle_path: &Path,
    context: &Context,
    work: F,
    cancellation: &CancellationToken,
    mut on_progress: P,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
    P: FnMut(Progress),
{
    let repository = Repository {
        repository_path: bottle_path.display().to_string(),
        block_size: FVS_BLOCK_SIZE,
    };
    let stream = context
        .fvs()
        .await?
        .commit_stream(&repository, AUTO_CHECKPOINT_MESSAGE.into())
        .await?;
    let checkpoint = finish_commit(stream, |progress| {
        on_progress(Progress::transferring(
            Stage::Checkpointing,
            progress.into(),
        ));
    })
    .await?;

    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }

    let result = work.await;
    let result = if result.is_ok() && cancellation.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        result
    };
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            let restored = async {
                let client = context.fvs().await?;
                let stream = client
                    .restore_stream(
                        &repository,
                        &checkpoint.state_id,
                        None::<&Path>,
                        true,
                        false,
                    )
                    .await?;
                finish_restore(stream, |progress| {
                    on_progress(Progress::transferring(Stage::Restoring, progress.into()));
                })
                .await
            }
            .await;
            if let Err(failed) = restored {
                tracing::error!(%failed, "prefix rollback failed after {error}");
            }
            Err(error)
        }
    }
}

/// Runs a prefix mutation directly when FVS rollback support is not compiled in.
#[cfg(not(feature = "fvs"))]
async fn transact<F, T, P>(
    _bottle_path: &Path,
    _context: &Context,
    work: F,
    _cancellation: &CancellationToken,
    _on_progress: P,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
    P: FnMut(Progress),
{
    work.await
}

/// Drains an FVS commit stream, forwarding every frame and requiring a terminal commit.
#[cfg(feature = "fvs")]
pub(crate) async fn finish_commit(
    stream: impl Stream<Item = std::result::Result<FvsProgress, FvsError>>,
    on_progress: impl FnMut(&FvsProgress),
) -> Result<Commit> {
    finish_stream(
        stream,
        on_progress,
        |progress| progress.result_commit,
        "commit",
    )
    .await
}

/// Drains an FVS restore stream, forwarding every frame and requiring a terminal result.
#[cfg(feature = "fvs")]
pub(crate) async fn finish_restore(
    stream: impl Stream<Item = std::result::Result<FvsProgress, FvsError>>,
    on_progress: impl FnMut(&FvsProgress),
) -> Result<RestoreResponse> {
    finish_stream(
        stream,
        on_progress,
        |progress| progress.result_restore,
        "restore",
    )
    .await
}

/// Consumes the FVS streaming protocol and extracts its terminal payload.
///
/// Every frame, including the terminal frame, is forwarded to `on_progress`.
/// End-of-stream or a terminal frame without the expected payload is a protocol
/// error rather than successful completion.
#[cfg(feature = "fvs")]
async fn finish_stream<T>(
    stream: impl Stream<Item = std::result::Result<FvsProgress, FvsError>>,
    mut on_progress: impl FnMut(&FvsProgress),
    mut result: impl FnMut(FvsProgress) -> Option<T>,
    operation: &'static str,
) -> Result<T> {
    futures_util::pin_mut!(stream);
    while let Some(progress) = stream.try_next().await? {
        on_progress(&progress);
        if progress.done {
            return result(progress).ok_or(FvsError::MissingStreamResult(operation).into());
        }
    }
    Err(FvsError::MissingStreamResult(operation).into())
}

#[cfg(all(test, feature = "fvs"))]
mod fvs_tests {
    use std::io;

    use futures_util::stream;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn finish_commit_forwards_progress_and_returns_terminal_result() {
        futures_lite::future::block_on(async {
            let frames = [
                FvsProgress {
                    phase: "hashing".into(),
                    current: 1,
                    total: 2,
                    ..Default::default()
                },
                FvsProgress {
                    phase: "indexing".into(),
                    current: -1,
                    total: -1,
                    ..Default::default()
                },
                FvsProgress {
                    phase: "done".into(),
                    done: true,
                    result_commit: Some(Commit {
                        state_id: "checkpoint".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ];
            let mut updates = Vec::new();

            let commit = finish_commit(stream::iter(frames.map(Ok::<_, FvsError>)), |progress| {
                updates.push(Transfer::from(progress))
            })
            .await
            .unwrap();

            assert_eq!(
                updates,
                [
                    Transfer {
                        current: 1,
                        total: Some(2),
                    },
                    Transfer {
                        current: 0,
                        total: None,
                    },
                    Transfer {
                        current: 0,
                        total: None,
                    },
                ]
            );
            assert_eq!(commit.state_id, "checkpoint");
        });
    }

    #[test]
    #[ignore = "requires BOTTLES_TEST_FVS2D"]
    fn failed_transaction_restores_its_checkpoint() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let executable =
                    std::env::var_os("BOTTLES_TEST_FVS2D").expect("BOTTLES_TEST_FVS2D is required");
                let root = std::env::temp_dir().join(format!("bottles-next-{}", Uuid::new_v4()));
                let directories = crate::Directories::from_path(root.join("data")).unwrap();
                let socket = directories.runtime_dir().join("fvs2d.sock");
                let context =
                    crate::Context::for_test(directories.clone(), Some(executable.into())).unwrap();
                let bottle_path = directories.bottle(Uuid::new_v4());
                std::fs::create_dir_all(&bottle_path).unwrap();
                context
                    .fvs()
                    .await
                    .unwrap()
                    .new_repository(&bottle_path, FVS_BLOCK_SIZE)
                    .await
                    .unwrap();
                let file = bottle_path.join("value");
                async_fs::write(&file, "before").await.unwrap();

                let changed = file.clone();
                let result = transact(
                    &bottle_path,
                    &context,
                    async move {
                        async_fs::write(changed, "after").await?;
                        Err::<(), _>(io::Error::other("expected failure").into())
                    },
                    &CancellationToken::new(),
                    |_| {},
                )
                .await;

                assert!(result.is_err());
                assert_eq!(async_fs::read_to_string(file).await.unwrap(), "before");
                fvs_rs::Fvs2dClient::connect(socket)
                    .await
                    .unwrap()
                    .shutdown(fvs_rs::UnmountMode::Lazy)
                    .await
                    .unwrap();
                std::fs::remove_dir_all(root).unwrap();
            });
    }
}

#[cfg(all(test, not(feature = "fvs")))]
mod no_fvs_tests {
    use std::io;

    use super::*;

    #[test]
    fn transaction_runs_directly_and_propagates_failure() {
        futures_lite::future::block_on(async {
            let directories = crate::Directories::from_path(
                std::env::temp_dir().join(format!("bottles-next-{}", Uuid::new_v4())),
            )
            .unwrap();
            let context = crate::Context::for_test(directories, None).unwrap();
            let mut ran = false;

            let result = transact(
                Path::new("unused"),
                &context,
                async {
                    ran = true;
                    Err::<(), _>(io::Error::other("expected failure").into())
                },
                &CancellationToken::new(),
                |_| panic!("direct mutation must not report FVS progress"),
            )
            .await;

            assert!(ran);
            assert!(matches!(result, Err(crate::error::Error::Io(_))));
        });
    }
}
