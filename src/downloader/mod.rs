mod config;
mod handle;
mod manager;
mod progress;
mod types;
mod worker;

pub use config::{DownloadConfig, DownloadManagerConfig};
pub use handle::DownloadHandle;
pub use manager::DownloadManager;
pub use progress::DownloadProgress;
pub(self) use types::DownloadRequest;
pub use types::Status;
pub(self) use worker::download_thread;
