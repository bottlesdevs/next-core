use thiserror::Error;

#[cfg(feature = "fvs")]
pub use crate::bottle::error::VirgoError;
pub use crate::{
    addons::{AddonError, CatalogError, InstallerError},
    bottle::error::BottleError,
    credentials::CredentialError,
    runner::RunnerError,
    utils::archive::ArchiveError,
    winebridge::BridgeError,
};
#[cfg(feature = "fvs")]
use fvs_rs::error::Error as FvsError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("configuration: {0}")]
    Config(#[from] next_config::error::Error),
    #[error("gRPC transport: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("gRPC status: {0}")]
    Status(#[from] tonic::Status),
    #[error("WineBridge error: {0}")]
    Bridge(#[from] BridgeError),
    #[error("Runner error: {0}")]
    Runner(#[from] RunnerError),
    #[cfg(feature = "fvs")]
    #[error("FVS error: {0}")]
    Fvs(#[from] FvsError),
    #[error("Bottle error: {0}")]
    Bottle(#[from] BottleError),
    #[cfg(feature = "fvs")]
    #[error("Virgo error: {0}")]
    Virgo(#[from] VirgoError),
    #[error("addon error: {0}")]
    Addon(#[from] AddonError),
    #[error("credential error: {0}")]
    Credential(#[from] CredentialError),
    #[cfg(feature = "fvs")]
    #[error("fvs2d executable is not configured")]
    Fvs2dNotConfigured,
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
pub trait ResultExt<T, E> {
    fn log_error(self) -> Option<T>;
    fn log_warn(self) -> Option<T>;
    fn log_info(self) -> Option<T>;
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
