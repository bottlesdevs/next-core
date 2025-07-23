use super::DownloadProgress;
use crate::Error;
use reqwest::Url;
use std::path::PathBuf;
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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Status {
    Queued,
    InProgress(DownloadProgress),
    Retrying,
    Completed,
    Failed,
    Cancelled,
}
