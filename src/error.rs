use thiserror::Error;

pub use crate::{
    bottle::error::{BottleError, VirgoError},
    compatibility::installer::InstallerError,
    runner::RunnerError,
    utils::archive::ArchiveError,
    winebridge::BridgeError,
};
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
    #[error("FVS error: {0}")]
    Fvs(#[from] FvsError),
    #[error("Bottle error: {0}")]
    Bottle(#[from] BottleError),
    #[error("Virgo error: {0}")]
    Virgo(#[from] VirgoError),
    #[error("archive error: {0}")]
    Archive(#[from] ArchiveError),
    #[error("installer error: {0}")]
    Installer(#[from] InstallerError),
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
