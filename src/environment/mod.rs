//! Coordinates environment state, persistence, runtime control, and storage.
//!
//! An [`Environment`] exposes immutable [`EnvironmentState`] snapshots, which
//! every environment publishes into one shared [`Inventory`]. Mutations are
//! serialized by one control lock per environment and publish a replacement
//! snapshot only after persistence succeeds. Deletion removes the snapshot from
//! the inventory without invalidating snapshots already held by callers.

mod backend;
mod edit;
mod error;
mod handle;
#[cfg(feature = "fvs")]
mod history;
mod runtime;
mod state;

pub use backend::PrefixBackend;
#[cfg(feature = "fvs")]
pub(crate) use backend::virgo::VirgoManager;
pub use edit::Edit;
pub use error::EnvironmentError;
pub use handle::Environment;
#[cfg(feature = "fvs")]
pub use history::{Snapshot, SnapshotSummary};
pub use state::EnvironmentState;

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

/// Every environment's published state, keyed by environment identifier.
pub type Inventory = Arc<BTreeMap<Uuid, Arc<EnvironmentState>>>;

pub(crate) struct Inner {
    pub(crate) id: Uuid,
    pub(crate) inventory: watch::Sender<Inventory>,
    pub(crate) control: Mutex<()>,
    pub(crate) root: PathBuf,
    pub(crate) context: Context,
    #[cfg(feature = "fvs")]
    pub(crate) virgo: Arc<VirgoManager>,
}

impl Inner {
    /// Constructs the environment stored at `root` without touching storage.
    ///
    /// Its state is read from `inventory`, where it is absent until published.
    ///
    /// # Errors
    ///
    /// Returns an error if `root` is not named by a UUID.
    pub(crate) fn new(
        root: PathBuf,
        inventory: watch::Sender<Inventory>,
        context: Context,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Result<Arc<Self>> {
        let id = root
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<Uuid>().ok())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "environment directory must be a UUID",
                )
            })?;
        Ok(Arc::new(Self {
            id,
            inventory,
            control: Mutex::new(()),
            root,
            context,
            #[cfg(feature = "fvs")]
            virgo,
        }))
    }

    /// Initializes storage for `state` and persists it without publishing it.
    ///
    /// Failure during backend initialization can leave a partial environment
    /// root and any already-published shared Virgo artifacts. After
    /// initialization succeeds, cancellation or a failed save moves the
    /// environment root to trash on a best-effort basis.
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
        state: &EnvironmentState,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        progress.send_replace(Some(Progress::new(Stage::CreatingPrefix)));
        // Initialization owns shutdown. Retain storage when it cannot finish safely.
        match state.backend {
            PrefixBackend::Standard => {
                standard::create(state, &self.root, &self.context).await?;
            }
            #[cfg(feature = "fvs")]
            PrefixBackend::Virgo => {
                let (base, overlays) = self
                    .virgo
                    .prepare_artifacts(state, progress, cancellation)
                    .await?;
                self.virgo
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
                async_fs::rename(&self.root, trash.join("environment"))
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
    /// Returns [`EnvironmentError::Deleted`] once the environment is absent from
    /// the inventory.
    pub(crate) fn state(&self) -> Result<Arc<EnvironmentState>> {
        self.inventory
            .borrow()
            .get(&self.id)
            .cloned()
            .ok_or_else(|| EnvironmentError::Deleted.into())
    }

    /// Streams the current state and later distinct publications until deletion.
    pub(crate) fn watch(
        &self,
    ) -> impl Stream<Item = Arc<EnvironmentState>> + Send + 'static + use<> {
        let id = self.id;
        let mut previous: Option<Arc<EnvironmentState>> = None;
        WatchStream::new(self.inventory.subscribe())
            .map(move |inventory| inventory.get(&id).cloned())
            .take_while(Option::is_some)
            .filter_map(move |state| {
                let state = state?;
                // Changes to other environments republish this one unchanged.
                let changed = previous
                    .as_ref()
                    .is_none_or(|previous| !Arc::ptr_eq(previous, &state));
                previous = Some(state.clone());
                changed.then_some(state)
            })
    }

    /// Publishes `state` when it differs from the current snapshot.
    pub(crate) fn publish(&self, state: EnvironmentState) {
        self.inventory.send_if_modified(|inventory| {
            if inventory.get(&self.id).map(AsRef::as_ref) == Some(&state) {
                return false;
            }
            Arc::make_mut(inventory).insert(self.id, Arc::new(state));
            true
        });
    }

    /// Atomically saves `state` as the environment's `state.toml`.
    ///
    /// Failure before the final rename can leave `state.tmp` beside the previous
    /// configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or filesystem persistence fails.
    pub(crate) async fn save(&self, state: &EnvironmentState) -> Result<()> {
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
        self.inventory.send_modify(|inventory| {
            Arc::make_mut(inventory).remove(&self.id);
        });
        on_deleted();
        async_fs::remove_dir_all(destination).await.log_warn();
        Ok(())
    }
}
