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

use crate::{Directories, error::Result};
use fvs_rs::Fvs2dClient;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

/// FVS repository block size used for Virgo artifacts and prefix history.
pub(crate) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;

/// The `fvs2d` client and the layer store built on it.
pub(crate) struct Fvs {
    pub(crate) client: Arc<Fvs2dClient>,
    pub(crate) layers: LayerStore,
}

impl Fvs {
    /// Connects to `fvs2d`, starting `executable` when no daemon is listening.
    ///
    /// An unset `executable` resolves `fvs2d` through `PATH`; a relative one is
    /// resolved against the process's current directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the current directory cannot be read or the daemon
    /// cannot be started or contacted.
    pub(crate) async fn connect(
        executable: Option<PathBuf>,
        directories: &Directories,
    ) -> Result<Self> {
        let client = Fvs2dClient::connect_or_spawn(
            daemon(executable)?,
            directories.runtime_dir().join("fvs2d.sock"),
        )
        .await?;
        Ok(Self::new(Arc::new(client), directories))
    }

    fn new(client: Arc<Fvs2dClient>, directories: &Directories) -> Self {
        Self {
            layers: LayerStore::new(directories.clone(), client.clone()),
            client,
        }
    }

    #[cfg(test)]
    /// Creates a client whose channel connects lazily, for tests that never issue RPCs.
    ///
    /// # Panics
    ///
    /// Panics if the process cannot construct the test-only Tokio runtime used
    /// to create the channel.
    pub(crate) fn for_test(directories: &Directories) -> Self {
        static RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> =
            std::sync::LazyLock::new(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
            });
        let _entered = RUNTIME.enter();
        let client = Fvs2dClient::from_channel(
            tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy(),
        );
        Self::new(Arc::new(client), directories)
    }
}

/// Resolves the `fvs2d` executable to start.
///
/// Relative paths are joined to the process's current directory. Collecting the
/// path components removes redundant separators and current-directory components,
/// but preserves parent-directory components; symbolic links are not resolved.
fn daemon(configured: Option<PathBuf>) -> Result<PathBuf> {
    let Some(path) = configured else {
        return Ok(PathBuf::from("fvs2d"));
    };
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(path.components().collect())
}

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

    fn root(&self) -> PathBuf {
        self.directories.data_dir().join("virgo")
    }

    fn staging_path(&self) -> PathBuf {
        self.directories.staging().join(Uuid::new_v4().to_string())
    }
}

/// Describes invalid artifacts and workspace lifecycle failures in Virgo storage.
#[derive(Debug, thiserror::Error)]
pub enum VirgoError {
    /// No locally acquired Soda runner has a parseable semantic version.
    #[error("no locally recorded Soda runner with a valid semantic version")]
    SodaNotDownloaded,
    /// No locally acquired WineBridge has a parseable semantic version.
    #[error("no locally recorded WineBridge with a valid semantic version")]
    WineBridgeNotDownloaded,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_path_lookup_when_fvs2d_is_not_configured() {
        assert_eq!(daemon(None).unwrap(), PathBuf::from("fvs2d"));
    }
}
