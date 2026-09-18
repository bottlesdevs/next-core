//! Wine layer storage and registry mechanics, independent of owner and addon policy.
//!
//! One Virgo manager owns one store per artifact root. Context shares the FVS client
//! initialized during core startup; this subsystem neither connects nor starts it.
//! Relative artifact addresses and composition order come from prefix policy.
//!
//! Published filesystem revisions and registry artifacts are immutable. get_or_build
//! holds coordination through input resolution, preparation, execution and finalization.
//! Callers stop processes before passing the execution result to finish_build. Failed
//! shutdown retains staging and any mount; dropping a workspace performs no cleanup.
//!
//! Composition preserves private registry changes but does not own checkpoints or
//! recovery. Callers prepare stopped, unmounted workspaces before publication. Explicit
//! deletion requires callers to know that an artifact is unmounted and no longer used;
//! the store does not track owners or collect garbage.

mod build;
mod cache;
mod registry;
mod workspace;
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
