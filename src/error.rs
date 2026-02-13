use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Download failed: {0}")]
    Download(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),
    #[error("Task not found: {0}")]
    TaskNotFound(String),
    #[error("Queue is closed")]
    QueueClosed,
    #[error("Task already exists: {0}")]
    TaskExists(String),
    #[error("Invalid state transition from {from} to {to}")]
    InvalidStateTransition { from: String, to: String },
    #[error("Worker error: {0}")]
    Worker(String),
    #[error("Retry limit exceeded for task: {0}")]
    RetryLimitExceeded(String),
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
}
