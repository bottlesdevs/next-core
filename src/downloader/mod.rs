mod config;
mod handle;
mod manager;
mod progress;
mod request;
mod worker;

pub use config::{DownloadConfig, DownloadManagerConfig};
pub use handle::DownloadHandle;
pub use manager::DownloadManager;
pub use progress::{DownloadProgress, Status};
pub use request::{DownloadBuilder, DownloadRequest};
