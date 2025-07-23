use super::{DownloadConfig, DownloadHandle, DownloadManager, DownloadProgress, Status};
use crate::{error::DownloadError, Error};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::{
    fs::File,
    sync::{oneshot, watch},
};
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Debug)]
pub struct DownloadRequest {
    url: Url,
    destination: PathBuf,
    pub result: oneshot::Sender<Result<File, Error>>,
    pub status: watch::Sender<Status>,
    pub cancel: CancellationToken,
    config: DownloadConfig,

    // For rate limiting
    last_progress_update: Instant,
}

impl DownloadRequest {
    pub fn new(
        url: Url,
        destination: impl AsRef<Path>,
        cancel: CancellationToken,
        config: DownloadConfig,
    ) -> (Self, DownloadHandle) {
        let (result_tx, result_rx) = oneshot::channel();
        let (status_tx, status_rx) = watch::channel(Status::Queued);
        (
            Self {
                url,
                destination: destination.as_ref().to_path_buf(),
                result: result_tx,
                status: status_tx,
                cancel: cancel.clone(),
                config,
                last_progress_update: Instant::now(),
            },
            DownloadHandle::new(result_rx, status_rx, cancel),
        )
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn destination(&self) -> &Path {
        &self.destination
    }

    pub fn config(&self) -> &DownloadConfig {
        &self.config
    }

    pub fn send_progress(&mut self, progress: DownloadProgress) {
        if self.last_progress_update.elapsed() >= self.config.progress_update_interval() {
            self.last_progress_update = Instant::now();
            self.status.send(Status::InProgress(progress)).ok();
        }
    }
}

#[derive(Debug)]
pub struct DownloadBuilder<'a> {
    manager: &'a DownloadManager,
    url: Url,
    destination: PathBuf,
    config: DownloadConfig,
}

impl<'a> DownloadBuilder<'a> {
    pub fn new(manager: &'a DownloadManager, url: Url, destination: impl AsRef<Path>) -> Self {
        Self {
            manager,
            url,
            destination: destination.as_ref().to_path_buf(),
            config: DownloadConfig::default(),
        }
    }

    pub fn with_retries(mut self, retries: usize) -> Self {
        self.config = self.config.with_max_retries(retries);
        self
    }

    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.config = self.config.with_user_agent(user_agent);
        self
    }

    /// Set how often progress updates are sent
    pub fn with_progress_interval(mut self, interval: Duration) -> Self {
        self.config = self.config.with_progress_interval(interval);
        self
    }

    pub fn with_config(mut self, config: DownloadConfig) -> Self {
        self.config = config;
        self
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn config(&self) -> &DownloadConfig {
        &self.config
    }

    pub fn start(self) -> Result<DownloadHandle, Error> {
        if self.manager.is_cancelled() {
            return Err(Error::Download(DownloadError::ManagerShutdown));
        }

        if self.destination.exists() {
            return Err(Error::Download(DownloadError::FileExists {
                path: self.destination,
            }));
        }

        let cancel = self.manager.child_token();
        let (req, handle) = DownloadRequest::new(self.url, self.destination, cancel, self.config);

        self.manager.queue_request(req)?;

        Ok(handle)
    }
}
