//! Names the filesystem locations owned by Bottles core.
//!
//! Every location is rooted at the application data and runtime directories
//! resolved by `next-paths`. Staging, trash, and Virgo directories are created
//! lazily by the workflows that use them.

use std::path::PathBuf;

use next_paths::{data_dir, runtime_dir};

use crate::error::Result;

/// Creates the directories needed at startup.
///
/// Local data, runtime, prefix, and addon-release directories are created
/// recursively.
///
/// # Errors
///
/// Returns an I/O error if a required directory cannot be created.
pub(crate) async fn create() -> Result<()> {
    for directory in [
        data_dir().to_path_buf(),
        runtime_dir().to_path_buf(),
        prefixes(),
        releases(),
    ] {
        async_fs::create_dir_all(directory).await?;
    }
    Ok(())
}

/// Returns the root used for disposable installation and publication workspaces.
///
/// The path is placed under the data directory, allowing completed trees to be
/// atomically renamed into managed data directories on the same filesystem.
pub(crate) fn staging() -> PathBuf {
    data_dir().join(".staging")
}

pub(crate) fn trash() -> PathBuf {
    data_dir().join(".trash")
}

pub(crate) fn prefixes() -> PathBuf {
    data_dir().join("prefixes")
}

pub(crate) fn releases() -> PathBuf {
    data_dir().join("releases")
}

pub(crate) fn catalogs() -> PathBuf {
    data_dir().join("catalogs.json")
}

#[cfg(feature = "fvs")]
pub(crate) fn virgo() -> PathBuf {
    data_dir().join("virgo")
}

#[cfg(feature = "fvs")]
pub(crate) fn fvs2d_socket() -> PathBuf {
    runtime_dir().join("fvs2d.sock")
}
