use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("gRPC transport: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("gRPC status: {0}")]
    Status(#[from] tonic::Status),
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
