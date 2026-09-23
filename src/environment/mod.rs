//! Shared ownership, publication and persistence for environment-backed handles.

mod backend;
mod edit;
mod error;
#[cfg(feature = "fvs")]
mod history;
mod runtime;
mod state;

pub use backend::PrefixBackend;
#[cfg(feature = "fvs")]
pub(crate) use backend::virgo::VirgoManager;
pub use edit::Edit;
pub use error::EnvironmentError;
#[cfg(feature = "fvs")]
pub use history::{Snapshot, SnapshotSummary};
pub use state::{EnvironmentConfig, State};

use crate::{
    Context, Progress, Stage,
    error::{Error, Result, ResultExt},
    utils::fs,
};
use backend::standard;
use futures_core::Stream;
use next_config::Config;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, MutexGuard, watch};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) trait BackendSource {
    fn backend(&self) -> PrefixBackend;
}

pub(crate) struct Environment<T> {
    pub(crate) published: watch::Sender<Option<Arc<State<T>>>>,
    pub(crate) control: Mutex<()>,
    pub(crate) root: PathBuf,
    pub(crate) context: Context,
    #[cfg(feature = "fvs")]
    pub(crate) virgo: Arc<VirgoManager>,
}

impl<T: BackendSource> Environment<T>
where
    State<T>: Config + Clone + PartialEq + Send + Sync,
{
    pub(crate) fn from_state(
        state: State<T>,
        root: PathBuf,
        context: Context,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Result<Arc<Self>> {
        state.config.validate()?;
        let expected = root
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<Uuid>().ok())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "environment directory must be a UUID",
                )
            })?;
        if state.id() != expected {
            return Err(EnvironmentError::IdMismatch {
                expected,
                actual: state.id(),
            }
            .into());
        }
        let (published, _) = watch::channel(Some(Arc::new(state)));
        Ok(Arc::new(Self {
            published,
            control: Mutex::new(()),
            root,
            context,
            #[cfg(feature = "fvs")]
            virgo,
        }))
    }

    /// Creation is private until initialization and persistence both succeed.
    pub(crate) async fn create(
        state: State<T>,
        root: PathBuf,
        context: Context,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<Arc<Self>> {
        let environment = Self::from_state(
            state,
            root,
            context,
            #[cfg(feature = "fvs")]
            virgo,
        )?;
        let state = environment.state()?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        progress.send_replace(Some(Progress::new(Stage::CreatingPrefix)));
        // Initialization owns shutdown. Retain storage when it cannot finish safely.
        match state.data.backend() {
            PrefixBackend::Standard => {
                standard::create(&state.config, &environment.root, &environment.context).await?;
            }
            #[cfg(feature = "fvs")]
            PrefixBackend::Virgo => {
                let (base, overlays) = environment
                    .virgo
                    .prepare_artifacts(&state.config, progress, cancellation)
                    .await?;
                environment
                    .virgo
                    .layers
                    .prepare_workspace(&environment.root, &base, &overlays, cancellation)
                    .await?;
            }
        }
        let result = async {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.send_replace(Some(Progress::new(Stage::Configuring)));
            environment.save(&state).await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            fs::with_temp_dir(&environment.context.directories().trash(), |trash| {
                async_fs::rename(&environment.root, trash.join("environment"))
            })
            .await
            .log_warn();
        }
        result?;
        Ok(environment)
    }

    pub(crate) fn state(&self) -> Result<Arc<State<T>>> {
        self.published
            .borrow()
            .clone()
            .ok_or_else(|| EnvironmentError::Deleted.into())
    }

    pub(crate) fn watch(&self) -> impl Stream<Item = Arc<State<T>>> + Send + 'static + use<T> {
        WatchStream::new(self.published.subscribe())
            .take_while(Option::is_some)
            .filter_map(|state| state)
    }

    pub(crate) fn publish(&self, state: State<T>) {
        let next = Arc::new(state);
        self.published.send_if_modified(|published| {
            if published.as_deref() == Some(next.as_ref()) {
                return false;
            }
            *published = Some(next);
            true
        });
    }

    pub(crate) async fn save(&self, state: &State<T>) -> Result<()> {
        Ok(next_config::save(self.root.join("state.toml"), state).await?)
    }

    pub(crate) async fn lock_control(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<MutexGuard<'_, ()>> {
        let guard = cancellation
            .run_until_cancelled(self.control.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(guard)
    }

    /// Stop and move the owner into global trash, then publish deletion.
    /// Notify the manager synchronously before awaiting best-effort cleanup.
    pub(crate) async fn delete(
        &self,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
        on_deleted: impl FnOnce(),
    ) -> Result<()> {
        let _control = self.lock_control(cancellation).await?;
        progress.send_replace(Some(Progress::new(Stage::Stopping)));
        self.stop_locked().await?;
        let trash = self.context.directories().trash();
        async_fs::create_dir_all(&trash).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        progress.send_replace(Some(Progress::new(Stage::Removing)));
        let destination = trash.join(Uuid::new_v4().to_string());
        async_fs::rename(&self.root, &destination).await?;
        self.published.send_replace(None);
        on_deleted();
        async_fs::remove_dir_all(destination).await.log_warn();
        Ok(())
    }
}
