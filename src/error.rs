//! Errors returned by the public core API.
//!
//! [`enum@Error`] preserves subsystem-specific causes for matching and error-chain
//! inspection.

pub use bottles_plugin_host::PluginError;
use thiserror::Error;

#[cfg(feature = "fvs")]
pub use crate::virgo::VirgoError;
pub use crate::{
    addons::{AddonError, CatalogError, InstallerError},
    bottle::BottleError,
    environment::EnvironmentError,
    profiles::ProfileError,
    runner::RunnerError,
    utils::fs::archive::ArchiveError,
    winebridge::BridgeError,
};
#[cfg(feature = "fvs")]
use fvs_rs::error::Error as FvsError;

/// A core operation result using [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level failures produced by Bottles core services.
///
/// Variants retain their subsystem error as a source wherever one is
/// available, so callers can inspect the error chain or match a specific
/// category.
#[derive(Error, Debug)]
pub enum Error {
    /// The operating system did not provide application data directories.
    #[error("application directories are unavailable on this platform")]
    ProjectDirectoriesUnavailable,
    /// A filesystem or process I/O operation failed.
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization or deserialization failed.
    #[error("Serde: {0}")]
    Serde(#[from] serde_json::Error),
    /// A persisted configuration could not be read, validated, or written.
    #[error("configuration: {0}")]
    Config(#[from] next_config::error::Error),
    /// A gRPC channel could not be established.
    #[error("gRPC transport: {0}")]
    Transport(#[from] tonic::transport::Error),
    /// A gRPC service returned a failing status.
    #[error("gRPC status: {0}")]
    Status(#[from] tonic::Status),
    /// `WineBridge` startup, discovery, or protocol handling failed.
    #[error("WineBridge error: {0}")]
    Bridge(#[from] BridgeError),
    /// A Wine or Proton runner failed.
    #[error("Runner error: {0}")]
    Runner(#[from] RunnerError),
    #[cfg(feature = "fvs")]
    /// The FVS client or daemon failed.
    #[error("FVS error: {0}")]
    Fvs(#[from] FvsError),
    /// A bottle-specific request was invalid.
    #[error("Bottle error: {0}")]
    Bottle(#[from] BottleError),
    /// Environment state, lifecycle, or validation failed.
    #[error("Environment error: {0}")]
    Environment(#[from] EnvironmentError),
    #[cfg(feature = "fvs")]
    /// Virgo layered storage failed.
    #[error("Virgo error: {0}")]
    Virgo(#[from] VirgoError),
    /// Addon catalog, download, installation, or storage failed.
    #[error("addon error: {0}")]
    Addon(#[from] AddonError),
    /// Profile or account management failed.
    #[error("profile error: {0}")]
    Profile(#[from] ProfileError),
    /// A plugin could not be loaded or contacted.
    #[error("plugin error: {0}")]
    Plugin(#[from] PluginError),
    /// A library provider rejected enumeration or launch.
    #[error("library provider {provider}: {message}")]
    LibraryProvider {
        /// Stable identifier of the provider that failed.
        provider: String,
        /// Provider-supplied diagnostic message.
        message: String,
    },
    /// The operating system credential store failed.
    #[error("credential error: {0}")]
    Credential(#[from] keyring::Error),
    /// Cooperative cancellation was observed at an operation-defined checkpoint.
    #[error("operation cancelled")]
    Cancelled,
}

impl From<CatalogError> for Error {
    fn from(error: CatalogError) -> Self {
        AddonError::from(error).into()
    }
}

impl From<InstallerError> for Error {
    fn from(error: InstallerError) -> Self {
        AddonError::from(error).into()
    }
}

impl From<ArchiveError> for Error {
    fn from(error: ArchiveError) -> Self {
        AddonError::from(error).into()
    }
}

impl From<download_manager::error::Error> for Error {
    fn from(error: download_manager::error::Error) -> Self {
        AddonError::from(error).into()
    }
}

#[allow(dead_code)]
/// Logs a failed [`Result`](std::result::Result) and converts it to an [`Option`].
///
/// Successful values become `Some`; failures are emitted through `tracing` at
/// the selected level and become `None`.
pub trait ResultExt<T, E> {
    /// Logs an error at `ERROR` level and returns the successful value.
    fn log_error(self) -> Option<T>;

    /// Logs an error at `WARN` level and returns the successful value.
    fn log_warn(self) -> Option<T>;

    /// Logs an error at `INFO` level and returns the successful value.
    fn log_info(self) -> Option<T>;

    /// Logs an error at `DEBUG` level and returns the successful value.
    fn log_debug(self) -> Option<T>;
}

impl<T, E: std::error::Error> ResultExt<T, E> for std::result::Result<T, E> {
    fn log_error(self) -> Option<T> {
        self.inspect_err(|e| tracing::error!("{e}")).ok()
    }

    fn log_warn(self) -> Option<T> {
        self.inspect_err(|e| tracing::warn!("{e}")).ok()
    }

    fn log_info(self) -> Option<T> {
        self.inspect_err(|e| tracing::info!("{e}")).ok()
    }

    fn log_debug(self) -> Option<T> {
        self.inspect_err(|e| tracing::debug!("{e}")).ok()
    }
}
