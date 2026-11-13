//! Coordinates prefix state, persistence, runtime control, and storage.
//!
//! A [`Prefix`] exposes immutable [`PrefixState`] snapshots. Mutations are
//! serialized by one control lock and publish a replacement snapshot only
//! after persistence succeeds. Deletion closes the publication stream without
//! invalidating snapshots already held by callers.

mod edit;
mod error;
mod handle;
#[cfg(feature = "fvs")]
mod history;
mod runtime;
mod startup;
mod state;
mod storage;

pub use edit::Edit;
pub use error::PrefixError;
pub use handle::Prefix;
#[cfg(feature = "fvs")]
pub use history::{Snapshot, SnapshotSummary};
pub use state::PrefixState;
pub use storage::PrefixBackend;

use crate::{
    Job, Stage,
    error::{Result, ResultExt},
    utils::fs,
};
use futures_core::Stream;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use storage::Storage;
pub(crate) use storage::Storages;
use tokio::sync::{Mutex, watch};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use uuid::Uuid;

/// Every prefix's published state, keyed by prefix identifier.
pub type Inventory = Arc<HashMap<Uuid, Arc<PrefixState>>>;

pub(crate) struct Inner {
    pub(crate) id: Uuid,
    pub(crate) state: watch::Sender<Option<Arc<PrefixState>>>,
    pub(crate) control: Mutex<()>,
    /// Notified each time this prefix starts `WineBridge`.
    pub(crate) bridge_started: watch::Sender<()>,
    pub(crate) root: PathBuf,
    /// Absent when this build lacks the prefix's backend.
    storage: Option<Arc<dyn Storage>>,
    /// FVS services used by prefix history.
    #[cfg_attr(not(feature = "fvs"), allow(dead_code))]
    fvs: Arc<crate::virgo::Fvs>,
}

impl Inner {
    /// Constructs prefix `id`, stored at `root`, around `state` without touching storage.
    pub(crate) fn new(
        id: Uuid,
        state: PrefixState,
        root: PathBuf,
        storage: Option<Arc<dyn Storage>>,
        fvs: Arc<crate::virgo::Fvs>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            storage,
            state: watch::channel(Some(Arc::new(state))).0,
            control: Mutex::new(()),
            bridge_started: watch::channel(()).0,
            root,
            fvs,
        })
    }
}

impl Prefix {
    /// Returns this build's storage for the prefix's backend.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::BackendUnavailable`] when this build lacks the
    /// backend.
    fn storage(&self, state: &PrefixState) -> Result<&dyn Storage> {
        self.0
            .storage
            .as_deref()
            .ok_or_else(|| PrefixError::BackendUnavailable(state.backend).into())
    }

    /// Initializes storage for the constructed state and persists it.
    ///
    /// Failure during backend initialization can leave a partial prefix
    /// root and any already-published shared Virgo artifacts. After
    /// initialization succeeds, cancellation or a failed save moves the
    /// prefix root to trash on a best-effort basis.
    ///
    /// Cancellation is sampled before backend initialization and before the final
    /// save. Backend-specific work may add safe points, but in-flight
    /// filesystem and FVS requests are not forcibly interrupted.
    ///
    /// # Errors
    ///
    /// Returns cancellation, runner, storage, backend, or persistence errors
    /// encountered during initialization.
    pub(crate) async fn create(&self, job: &Job) -> Result<()> {
        let state = self.state()?;
        let state = state.as_ref();
        job.transition(Stage::CreatingPrefix)?;
        // Initialization owns shutdown. Retain storage when it cannot finish safely.
        self.storage(state)?
            .create(state, &self.0.root, job)
            .await?;
        let result = async {
            job.transition(Stage::Configuring)?;
            self.save(state).await
        }
        .await;
        if result.is_err() {
            fs::discard(&self.0.root).await.log_warn();
        }
        result
    }

    /// Returns the currently published immutable state snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::Deleted`](crate::PrefixError::Deleted)
    /// after the prefix is deleted.
    pub fn state(&self) -> Result<Arc<PrefixState>> {
        self.0
            .state
            .borrow()
            .clone()
            .ok_or_else(|| PrefixError::Deleted.into())
    }

    /// Streams the current state and later published replacements.
    ///
    /// The first item is the state current at subscription time. Slow consumers
    /// may miss intermediate states, and deletion ends the stream.
    pub fn watch(&self) -> impl Stream<Item = Arc<PrefixState>> + Send + 'static + use<> {
        WatchStream::new(self.0.state.subscribe())
            .take_while(Option::is_some)
            .filter_map(|state| state)
    }

    /// Publishes `state` when it differs from the current snapshot.
    pub(crate) fn publish(&self, state: PrefixState) {
        self.0.state.send_if_modified(|current| {
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
        Ok(next_config::save(self.0.root.join("state.toml"), state).await?)
    }

    /// Stops the runtime, discards the root, and publishes deletion.
    ///
    /// `on_deleted` runs synchronously after the root is discarded and deletion
    /// is published. Cancellation is not sampled during runtime shutdown; it is
    /// checked again before the root is discarded.
    ///
    /// # Errors
    ///
    /// Returns cancellation, runtime shutdown, or filesystem errors that occur
    /// before the root has been withdrawn. Cleanup errors after withdrawal are logged.
    pub(crate) async fn delete(&self, job: &Job, on_deleted: impl FnOnce()) -> Result<()> {
        let _control = job.cancellable(self.0.control.lock()).await?;
        job.transition(Stage::Stopping)?;
        self.stop_locked().await?;
        job.transition(Stage::Removing)?;
        fs::discard(&self.0.root).await?;
        self.0.state.send_replace(None);
        on_deleted();
        Ok(())
    }
}
