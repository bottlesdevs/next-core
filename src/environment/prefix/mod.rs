//! Prefix storage backends.
//!
//! Standard storage mutates a conventional prefix directly; Virgo stores an
//! ordered FVS layer stack with a private writable upper directory. Virgo addon
//! changes use rollback checkpoints; Standard uses FVS only for explicit snapshots.

#[cfg(feature = "fvs")]
mod virgo;

use std::path::Path;

#[cfg(feature = "fvs")]
pub use virgo::VirgoError;

#[cfg(feature = "fvs")]
use fvs_rs::Layer;
use serde::{Deserialize, Serialize};

use crate::{Context, error::Result};

#[cfg(feature = "fvs")]
pub(crate) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;

/// Selects conventional mutable storage or FVS composition.
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub enum Storage {
    /// Stores a conventional mutable prefix in the owner directory.
    ///
    /// Explicit snapshots may use FVS; ordinary mutations use direct writes.
    Standard,
    /// Stores the prefix as composable FVS layers.
    ///
    /// Virgo is experimental and requires the configured FVS service.
    #[cfg(feature = "fvs")]
    Virgo {
        /// Exact immutable revisions derived from the environment selections.
        #[serde(default)]
        layers: Vec<Layer>,
    },
}

/// Creates storage at an explicit owner location.
pub(crate) async fn create(storage: &Storage, root: &Path) -> Result<()> {
    let directory = match storage {
        Storage::Standard => "prefix",
        #[cfg(feature = "fvs")]
        Storage::Virgo { .. } => "upper",
    };
    Ok(async_fs::create_dir_all(root.join(directory)).await?)
}

pub(crate) async fn prepare(storage: &Storage, root: &Path, context: &Context) -> Result<()> {
    let _ = (root, context);
    match storage {
        Storage::Standard => Ok(()),
        #[cfg(feature = "fvs")]
        Storage::Virgo { layers } => virgo::prepare(root, layers, context).await,
    }
}

pub(crate) async fn stop(storage: &Storage, root: &Path, context: &Context) -> Result<()> {
    let _ = (root, context);
    match storage {
        Storage::Standard => Ok(()),
        #[cfg(feature = "fvs")]
        Storage::Virgo { .. } => virgo::stop(root, context).await,
    }
}
