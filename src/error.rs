extern crate unix_named_pipe;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Failed to create named pipe")]
    NamedPipeError,
    #[error("Failed to connect to bridge, be sure to call .connect() first")]
    ConnectToBridgeError,
}
