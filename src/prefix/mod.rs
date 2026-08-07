//! Prefix persistence and transactional mutation.

mod standard;
mod virgo;

use std::{future::Future, path::Path};

use futures_core::Stream;
use futures_util::TryStreamExt;
use fvs_rs::{
    Commit, Layer, Progress as FvsProgress, Repository, RestoreResponse, error::Error as FvsError,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    Context, Progress, Stage, Transfer,
    bottle::Storage,
    error::{Error, Result},
    runner::Runner,
};

pub(crate) const AUTO_CHECKPOINT_MESSAGE: &str = "bottles-next:auto-checkpoint";
pub(crate) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub(crate) enum Prefix {
    Standard,
    Virgo {
        #[serde(default)]
        layers: Vec<Layer>,
    },
}

impl From<&FvsProgress> for Transfer {
    fn from(progress: &FvsProgress) -> Self {
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
        match storage {
            Storage::Standard => {
                standard::create(bottle_path, runner).await?;
                Ok(Self::Standard)
            }
            Storage::Virgo => Ok(Self::Virgo {
                layers: virgo::create(bottle_path, runner, runner_key, context).await?,
            }),
        }
    }

    pub(crate) fn kind(&self) -> Storage {
        match self {
            Self::Standard => Storage::Standard,
            Self::Virgo { .. } => Storage::Virgo,
        }
    }

    pub(crate) async fn prepare(&self, bottle_path: &Path, context: &Context) -> Result<()> {
        match self {
            Self::Standard => Ok(()),
            Self::Virgo { layers } => virgo::prepare(bottle_path, layers, context).await,
        }
    }

    pub(crate) async fn stop(&self, bottle_path: &Path, context: &Context) -> Result<()> {
        match self {
            Self::Standard => Ok(()),
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
        let Self::Virgo { layers } = self else {
            return Ok(());
        };
        virgo::rebuild(layers, runner, runner_key, installed, context).await
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
        let work = async {
            match self {
                Self::Standard => standard::install(bottle_path, execute).await,
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
        let work = async {
            match self {
                Self::Standard => standard::uninstall(bottle_path, execute).await,
                Self::Virgo { layers } => {
                    virgo::uninstall(bottle_path, layers, item_id, execute, context).await
                }
            }
        };
        transact(bottle_path, context, work, cancellation, on_progress).await
    }
}

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

#[cfg(test)]
mod tests {
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
                    crate::Context::for_test(directories.clone(), Some(executable.into()))
                        .await
                        .unwrap();
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
