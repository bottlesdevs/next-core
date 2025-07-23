use super::Status;
use crate::Error;
use tokio::{
    fs::File,
    sync::{oneshot, watch},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct DownloadHandle {
    result: oneshot::Receiver<Result<File, Error>>,
    status: watch::Receiver<Status>,
    cancel: CancellationToken,
}

impl DownloadHandle {
    pub fn new(
        result: oneshot::Receiver<Result<File, Error>>,
        status: watch::Receiver<Status>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            result,
            status,
            cancel,
        }
    }
}

impl std::future::Future for DownloadHandle {
    type Output = Result<tokio::fs::File, Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        use std::pin::Pin;
        use std::task::Poll;
        match Pin::new(&mut self.result).poll(cx) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(e)) => Poll::Ready(Err(Error::Oneshot(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl DownloadHandle {
    pub fn status(&self) -> Status {
        *self.status.borrow()
    }

    pub async fn wait_for_status_update(&mut self) -> Result<(), watch::error::RecvError> {
        self.status.changed().await
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}
