//! Coordinates environment state, persistence, runtime control, and storage.
//!
//! Bottles and standalone programs expose immutable [`State`] snapshots.
//! Mutations are serialized by one control lock and publish a replacement
//! snapshot only after persistence succeeds. Deletion closes the
//! publication stream without invalidating snapshots already held by callers.

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
    /// Constructs an owner around already-loaded state without touching storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration is invalid, `root` is not named by
    /// a UUID, or that UUID differs from [`State::id`].
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

    /// Initializes storage and persists a new owner before returning it.
    ///
    /// The owner remains private until both backend initialization and saving
    /// `state.toml` succeed. Failure during backend initialization can leave a
    /// partial owner root and any already-published shared Virgo artifacts. After
    /// initialization succeeds, cancellation or a failed save moves the owner
    /// root to trash on a best-effort basis.
    ///
    /// Cancellation is sampled before backend initialization and before and after
    /// the final save. Backend-specific work may add safe points, but in-flight
    /// filesystem and FVS requests are not forcibly interrupted.
    ///
    /// # Errors
    ///
    /// Returns configuration, cancellation, runner, storage, backend, or
    /// persistence errors encountered during initialization.
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

    /// Returns the currently published state snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::Deleted`] after deletion has been published.
    pub(crate) fn state(&self) -> Result<Arc<State<T>>> {
        self.published
            .borrow()
            .clone()
            .ok_or_else(|| EnvironmentError::Deleted.into())
    }

    /// Streams the current state and later distinct publications until deletion.
    pub(crate) fn watch(&self) -> impl Stream<Item = Arc<State<T>>> + Send + 'static + use<T> {
        WatchStream::new(self.published.subscribe())
            .take_while(Option::is_some)
            .filter_map(|state| state)
    }

    /// Publishes `state` when it differs from the current snapshot.
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

    /// Atomically saves `state` as the owner's `state.toml`.
    ///
    /// Failure before the final rename can leave `state.tmp` beside the previous
    /// configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or filesystem persistence fails.
    pub(crate) async fn save(&self, state: &State<T>) -> Result<()> {
        Ok(next_config::save(self.root.join("state.toml"), state).await?)
    }

    /// Acquires mutation coordination unless cancellation wins the wait.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cancelled`] if `cancellation` fires before or immediately
    /// after the lock is acquired.
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

    /// Stops the runtime, withdraws the root into trash, and publishes deletion.
    ///
    /// `on_deleted` runs synchronously after the successful rename and publication,
    /// before best-effort removal of the trashed directory.
    /// Cancellation is not sampled during runtime shutdown; it is checked again
    /// before the rename into trash.
    ///
    /// # Errors
    ///
    /// Returns cancellation, runtime shutdown, or filesystem errors that occur
    /// before the root has been withdrawn. Cleanup errors after withdrawal are logged.
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
