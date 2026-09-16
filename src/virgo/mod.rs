//! Wine layer storage and registry mechanics, independent of owner and addon policy.
//! A Virgo manager owns the store and shares the already-connected FVS client.

mod build;
mod cache;
mod registry;
mod workspace;
pub(crate) use build::{LayerBuild, Reservation};
pub(crate) use cache::VirgoLayer;

use fvs_rs::Fvs2dClient;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

pub(crate) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;

pub(crate) struct LayerStore {
    root: PathBuf,
    fvs: Arc<Fvs2dClient>,
    build_lock: Mutex<()>,
}

impl LayerStore {
    /// Construct without storage work. The caller owns connection setup.
    pub(crate) fn new(root: PathBuf, fvs: Arc<Fvs2dClient>) -> Self {
        Self {
            root,
            fvs,
            build_lock: Mutex::new(()),
        }
    }

    fn staging_path(&self) -> PathBuf {
        self.root.join(".staging").join(Uuid::new_v4().to_string())
    }
}

/// Virgo-specific failures carried by [`crate::error::Error::Virgo`].
#[derive(Debug, thiserror::Error)]
pub enum VirgoError {
    /// Virgo cannot mount a prefix over a nonempty mountpoint.
    #[error("mountpoint is not empty: {0}")]
    DirtyMountpoint(std::path::PathBuf),
    /// A selected artifact has not been built or has been removed.
    #[error("missing Virgo artifact: {0}")]
    MissingArtifact(std::path::PathBuf),
    /// A published artifact has an unsupported format or incomplete installed effects.
    #[error("invalid Virgo artifact: {0}")]
    InvalidArtifact(std::path::PathBuf),
    /// Registry data could not be converted while building a Virgo layer.
    #[error("failed to process Virgo registry data: {0}")]
    Registry(String),
    /// Storage is retained when its mount cannot be released.
    #[error("could not unmount {path}; workspace retained: {source}")]
    Unmount {
        path: PathBuf,
        #[source]
        source: fvs_rs::error::Error,
    },
}
