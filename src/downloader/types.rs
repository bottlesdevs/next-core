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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
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
