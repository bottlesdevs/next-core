mod config;
mod handle;
mod manager;
mod progress;
mod types;
mod worker;

pub use config::DownloadManagerConfig;
pub use handle::DownloadHandle;
pub use manager::*;
pub use progress::DownloadProgress;
pub use types::*;
pub(self) use worker::download_thread;
