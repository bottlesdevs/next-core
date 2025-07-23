use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Reqwest: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("Oneshot: {0}")]
    Oneshot(#[from] tokio::sync::oneshot::error::RecvError),
    #[error("Download: {0}")]
    Download(#[from] DownloadError),
}

#[derive(Error, Debug, Clone)]
pub enum DownloadError {
    #[error("Download was cancelled")]
    Cancelled,
    #[error("Retry limit exceeded: {last_error_msg}")]
    RetriesExhausted { last_error_msg: String },
    #[error("Download queue is full")]
    QueueFull,
    #[error("Download manager has been shut down")]
    ManagerShutdown,
    #[error("File already exists: {path}")]
    FileExists { path: PathBuf },
}
