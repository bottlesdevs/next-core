use super::{DownloadHandle, DownloadProgress};
use crate::Error;
use reqwest::Url;
use std::path::{Path, PathBuf};
use tokio::{
    fs::File,
    sync::{oneshot, watch},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct DownloadRequest {
    pub url: Url,
    pub destination: PathBuf,
    pub result: oneshot::Sender<Result<File, Error>>,
    pub status: watch::Sender<Status>,
    pub cancel: CancellationToken,
}

impl DownloadRequest {
    pub fn new_req_handle_pair(
        url: Url,
        destination: impl AsRef<Path>,
        cancel: CancellationToken,
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
            },
            DownloadHandle::new(result_rx, status_rx, cancel),
        )
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
