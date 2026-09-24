//! Builds, caches, and mounts immutable Wine filesystem and registry layers.
//!
//! Published artifacts are immutable. Build staging and mounts are retained when
//! cleanup has an uncertain outcome. The store does not track references or
//! collect unused artifacts, so removal requires proof that no workspace uses or
//! mounts the artifact.

mod build;
mod cache;
mod registry;
mod workspace;
pub(crate) use cache::VirgoLayer;

use crate::Directories;
use fvs_rs::Fvs2dClient;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

/// FVS repository block size used for Virgo artifacts and environment history.
pub(crate) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;

pub(crate) struct LayerStore {
    directories: Directories,
    fvs: Arc<Fvs2dClient>,
    build_lock: Mutex<()>,
}

impl LayerStore {
    /// Creates a store without touching storage or connecting the FVS client.
    pub(crate) fn new(directories: Directories, fvs: Arc<Fvs2dClient>) -> Self {
        Self {
            directories,
            fvs,
            build_lock: Mutex::new(()),
        }
    }

    fn staging_path(&self) -> PathBuf {
        self.directories.staging().join(Uuid::new_v4().to_string())
    }
}

/// Describes invalid artifacts and workspace lifecycle failures in Virgo storage.
#[derive(Debug, thiserror::Error)]
pub enum VirgoError {
    /// Mounting would hide files already present at the prefix mountpoint.
    #[error("mountpoint is not empty: {0}")]
    DirtyMountpoint(std::path::PathBuf),
    /// A selected artifact is absent from the cache.
    #[error("missing Virgo artifact: {0}")]
    MissingArtifact(std::path::PathBuf),
    /// An artifact manifest has the wrong UUID or an empty FVS commit identifier.
    #[error("invalid Virgo artifact: {0}")]
    InvalidArtifact(std::path::PathBuf),
    /// Registry parsing, diffing, or patch application failed.
    #[error("failed to process Virgo registry data: {0}")]
    Registry(String),
    /// A mount could not be released, so its workspace was retained.
    #[error("could not unmount {path}; workspace retained: {source}")]
    Unmount {
        /// Workspace path retained for recovery or diagnosis.
        path: PathBuf,
        /// Error returned by FVS while unmounting.
        #[source]
        source: fvs_rs::error::Error,
    },
}
