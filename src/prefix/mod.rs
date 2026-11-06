//! Coordinates prefix state, persistence, runtime control, and storage.
//!
//! A [`Prefix`] exposes immutable [`PrefixState`] snapshots. Mutations are
//! serialized by one control lock and publish a replacement snapshot only
//! after persistence succeeds. Deletion closes the publication stream without
//! invalidating snapshots already held by callers.

mod backend;
mod edit;
mod error;
mod handle;
#[cfg(feature = "fvs")]
mod history;
mod runtime;
mod state;

pub use backend::PrefixBackend;
pub use edit::Edit;
pub use error::PrefixError;
pub use handle::Prefix;
#[cfg(feature = "fvs")]
pub use history::{Snapshot, SnapshotSummary};
pub use state::PrefixState;

use crate::{
    Context, Progress, Stage,
    error::{Error, Result, ResultExt},
    utils::fs,
};
use backend::standard;
use futures_core::Stream;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, MutexGuard, watch};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Every prefix's published state, keyed by prefix identifier.
pub type Inventory = Arc<BTreeMap<Uuid, Arc<PrefixState>>>;

pub(crate) struct Inner {
    pub(crate) id: Uuid,
    pub(crate) state: watch::Sender<Option<Arc<PrefixState>>>,
    pub(crate) control: Mutex<()>,
    pub(crate) root: PathBuf,
    pub(crate) context: Context,
}

impl Inner {
    /// Constructs the prefix stored at `root` around `state` without touching storage.
    ///
    /// # Errors
    ///
    /// Returns an error if `root` is not named by a UUID.
    pub(crate) fn new(state: PrefixState, root: PathBuf, context: Context) -> Result<Arc<Self>> {
        let id = root
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<Uuid>().ok())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "prefix directory must be a UUID",
                )
            })?;
        Ok(Arc::new(Self {
            id,
            state: watch::channel(Some(Arc::new(state))).0,
            control: Mutex::new(()),
            root,
            context,
        }))
    }

    /// Initializes storage for the constructed state and persists it.
    ///
    /// Failure during backend initialization can leave a partial prefix
    /// root and any already-published shared Virgo artifacts. After
    /// initialization succeeds, cancellation or a failed save moves the
    /// prefix root to trash on a best-effort basis.
    ///
    /// Cancellation is sampled before backend initialization and before and after
    /// the final save. Backend-specific work may add safe points, but in-flight
    /// filesystem and FVS requests are not forcibly interrupted.
    ///
    /// # Errors
    ///
    /// Returns cancellation, runner, storage, backend, or persistence errors
    /// encountered during initialization.
    pub(crate) async fn create(
        &self,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let state = self.state()?;
        let state = state.as_ref();
        progress.send_replace(Some(Progress::new(Stage::CreatingPrefix)));
        // Initialization owns shutdown. Retain storage when it cannot finish safely.
        match state.backend {
            PrefixBackend::Standard => {
                standard::create(state, &self.root, &self.context).await?;
            }
            #[cfg(feature = "fvs")]
            PrefixBackend::Virgo => {
                let (base, overlays) = backend::virgo::Virgo::new(&self.context)
                    .prepare_artifacts(state, progress, cancellation)
                    .await?;
                self.context
                    .fvs()
                    .layers
                    .prepare_workspace(&self.root, &base, &overlays, cancellation)
                    .await?;
            }
        }
        let result = async {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.send_replace(Some(Progress::new(Stage::Configuring)));
            self.save(state).await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            fs::with_temp_dir(&self.context.directories().trash(), |trash| {
                async_fs::rename(&self.root, trash.join("prefix"))
            })
            .await
            .log_warn();
        }
        result
    }

    /// Returns the currently published state snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::Deleted`] after deletion has been published.
    pub(crate) fn state(&self) -> Result<Arc<PrefixState>> {
        self.state
            .borrow()
            .clone()
            .ok_or_else(|| PrefixError::Deleted.into())
    }

    /// Streams the current state and later publications until deletion.
    pub(crate) fn watch(&self) -> impl Stream<Item = Arc<PrefixState>> + Send + 'static + use<> {
        WatchStream::new(self.state.subscribe())
            .take_while(Option::is_some)
            .filter_map(|state| state)
    }

    /// Publishes `state` when it differs from the current snapshot.
    pub(crate) fn publish(&self, state: PrefixState) {
        self.state.send_if_modified(|current| {
            if current.as_deref() == Some(&state) {
                return false;
            }
            *current = Some(Arc::new(state));
            true
        });
    }

    /// Atomically saves `state` as the prefix's `state.toml`.
    ///
    /// Failure before the final rename can leave `state.tmp` beside the previous
    /// configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or filesystem persistence fails.
    pub(crate) async fn save(&self, state: &PrefixState) -> Result<()> {
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
        self.state.send_replace(None);
        on_deleted();
        async_fs::remove_dir_all(destination).await.log_warn();
        Ok(())
    }
}
