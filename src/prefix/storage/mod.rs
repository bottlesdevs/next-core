//! Prefix storage strategies.
//!
//! [`PrefixBackend`] records whether a prefix owns a conventional,
//! directly modified Wine prefix or a Virgo workspace assembled from immutable
//! layers. [`Storage`] is the behaviour each backend implements.

mod standard;
#[cfg(feature = "fvs")]
mod virgo;

use crate::{Job, PrefixState, error::Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

/// Materializes and maintains the Wine prefix below a prefix root.
///
/// The prefix must be stopped for every method except [`mount`](Self::mount),
/// which prepares a stopped prefix for startup.
#[async_trait]
pub(crate) trait Storage: Send + Sync {
    /// Creates storage for a new prefix described by `state`.
    ///
    /// Leaves no Wine processes running.
    async fn create(&self, state: &PrefixState, root: &Path, job: &Job) -> Result<()>;

    /// Returns whether moving from `before` to `after` changes stored contents.
    fn changes(&self, before: &PrefixState, after: &PrefixState) -> bool;

    /// Moves storage from `before` to `after`, leaving no Wine processes running.
    ///
    /// Called only when [`changes`](Self::changes) is true. A failure can leave
    /// storage partially changed; the caller is responsible for recovery.
    async fn apply(
        &self,
        before: &PrefixState,
        after: &PrefixState,
        root: &Path,
        job: &Job,
    ) -> Result<()>;

    /// Makes the Wine prefix usable before Wine starts.
    ///
    /// The caller must [`release`](Self::release) storage if mounting fails.
    async fn mount(&self, state: &PrefixState, root: &Path) -> Result<()>;

    /// Releases storage and discovery files after Wine has stopped.
    async fn release(&self, root: &Path) -> Result<()>;
}

/// The storage of every backend this build provides, shared by the prefixes
/// of one core.
#[derive(Clone)]
pub(crate) struct Storages {
    standard: Arc<dyn Storage>,
    /// Absent without the `fvs` feature.
    virgo: Option<Arc<dyn Storage>>,
}

impl Storages {
    #[cfg_attr(not(feature = "fvs"), allow(unused_variables))]
    pub(crate) fn new(addons: crate::Addons, fvs: Arc<crate::virgo::Fvs>) -> Self {
        Self {
            standard: Arc::new(standard::Standard),
            #[cfg(feature = "fvs")]
            virgo: Some(Arc::new(virgo::Virgo::new(addons, fvs))),
            #[cfg(not(feature = "fvs"))]
            virgo: None,
        }
    }

    /// Returns the storage implementing `backend`, if this build provides it.
    pub(crate) fn get(&self, backend: PrefixBackend) -> Option<Arc<dyn Storage>> {
        match backend {
            PrefixBackend::Standard => Some(self.standard.clone()),
            PrefixBackend::Virgo => self.virgo.clone(),
        }
    }
}

/// Selects how a prefix stores and materializes its Wine prefix.
///
/// This choice is persisted with the prefix and cannot be changed by an edit.
/// A build without the `fvs` feature still lists persisted Virgo prefixes and
/// can delete them, but cannot create, start, or change the software of one.
#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub enum PrefixBackend {
    /// Stores installed software and user data together in one mutable prefix.
    Standard,
    /// Composes immutable FVS layers over a private writable upper directory.
    ///
    /// Available only with the `fvs` crate feature; see
    /// [`is_available`](Self::is_available).
    Virgo,
}

impl PrefixBackend {
    /// Returns whether this build can create and start prefixes of this backend.
    pub fn is_available(self) -> bool {
        match self {
            Self::Standard => true,
            Self::Virgo => cfg!(feature = "fvs"),
        }
    }
}
