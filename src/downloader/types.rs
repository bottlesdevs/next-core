use super::{DownloadConfig, DownloadHandle, DownloadProgress};
use crate::{error::DownloadError, Error};
use reqwest::Url;
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
use tokio::{
    fs::File,
    sync::{oneshot, watch},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct DownloadRequest {
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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Status {
    Queued,
    InProgress(DownloadProgress),
    Retrying,
    Completed,
    Failed,
    Cancelled,
}
