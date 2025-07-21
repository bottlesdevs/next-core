use crate::{error::DownloadError, Error};
use reqwest::{Client, Url};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    fs::File,
    io::AsyncWriteExt,
    sync::{mpsc, oneshot, watch, Semaphore},
};

const QUEUE_SIZE: usize = 100;
const MAX_RETRIES: usize = 3;

#[derive(Debug)]
struct DownloadRequest {
    url: Url,
    destination: PathBuf,
    result: oneshot::Sender<Result<File, Error>>,
    status: watch::Sender<Status>,
    cancel: oneshot::Receiver<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Debug)]
pub struct DownloadHandle {
    result: oneshot::Receiver<Result<File, Error>>,
    status: watch::Receiver<Status>,
    cancel: oneshot::Sender<()>,
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

    pub fn cancel(self) {
        self.cancel.send(()).ok();
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Status {
    Pending,
    InProgress(DownloadProgress),
    Completed,
    Retrying,
    Failed,
}

#[derive(Debug)]
pub struct DownloadManager {
    queue: mpsc::Sender<DownloadRequest>,
    semaphore: Arc<Semaphore>,
}

impl Drop for DownloadManager {
    fn drop(&mut self) {
        // Need to manually close the semaphore to make sure dispatcher_thread stops waiting for permits
        self.semaphore.close();
    }
}

impl DownloadManager {
    pub fn new(limit: usize) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE_SIZE);
        let client = Client::new();
        let semaphore = Arc::new(Semaphore::new(limit));
        let manager = Self {
            queue: tx,
            semaphore: semaphore.clone(),
        };
        // Spawn the dispatcher thread to handle download requests
        tokio::spawn(async move { dispatcher_thread(client, rx, semaphore).await });
        manager
    }

    pub fn set_max_parallel_downloads(&self, limit: usize) {
        let current = self.semaphore.available_permits();
        if limit > current {
            self.semaphore.add_permits(limit - current);
        } else if limit < current {
            let to_remove = current - limit;
            for _ in 0..to_remove {
                let _ = self.semaphore.try_acquire();
            }
        }
    }

    pub fn add_request(&self, url: Url, destination: PathBuf) -> DownloadHandle {
        let (result_tx, result_rx) = oneshot::channel();
        let (status_tx, status_rx) = watch::channel(Status::Pending);
        let (cancel_tx, cancel_rx) = oneshot::channel();

        let req = DownloadRequest {
            url,
            destination,
            result: result_tx,
            status: status_tx,
            cancel: cancel_rx,
        };

        let _ = self.queue.try_send(req);

        DownloadHandle {
            result: result_rx,
            status: status_rx,
            cancel: cancel_tx,
        }
    }
}

async fn dispatcher_thread(
    client: Client,
    mut rx: mpsc::Receiver<DownloadRequest>,
    sem: Arc<Semaphore>,
) {
    while let Some(request) = rx.recv().await {
        let permit = match sem.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };
        let client = client.clone();
        tokio::spawn(async move {
            // Move the permit into the worker thread so it's automatically released when the thread finishes
            let _permit = permit;
            download_thread(client.clone(), request).await;
        });
    }
}

async fn download_thread(client: Client, mut req: DownloadRequest) {
    fn should_retry(e: &Error) -> bool {
        match e {
            Error::Reqwest(network_err) => {
                network_err.is_timeout()
                    || network_err.is_connect()
                    || network_err.is_request()
                    || network_err
                        .status()
                        .map(|status_code| status_code.is_server_error())
                        .unwrap_or(true)
            }
            Error::Download(DownloadError::Cancelled) | Error::Io(_) => false,
            _ => false,
        }
    }

    let mut last_error = None;
    for attempt in 0..=(MAX_RETRIES + 1) {
        if attempt > MAX_RETRIES {
            req.status.send(Status::Failed).ok();
            req.result
                .send(Err(Error::Download(DownloadError::RetriesExhausted {
                    last_error_msg: last_error
                        .as_ref()
                        .map(|e: &crate::Error| e.to_string())
                        .unwrap_or_else(|| "Unknown Error".to_string()),
                })))
                .ok();
            return;
        }

        if attempt > 0 {
            req.status.send(Status::Retrying).ok();
            // Basic exponential backoff
            let delay_ms = 1000 * 2u64.pow(attempt as u32 - 1);
            let delay = Duration::from_millis(delay_ms);

            tokio::select! {
                _ = tokio::time::sleep(delay) => {},
                _ = &mut req.cancel => {
                    req.status.send(Status::Failed).ok();
                    req.result.send(Err(Error::Download(DownloadError::Cancelled))).ok();
                    return;
                }
            }
        }

        match download(client.clone(), &mut req).await {
            Ok(file) => {
                req.status.send(Status::Completed).ok();
                req.result.send(Ok(file)).ok();
                return;
            }
            Err(e) => {
                if should_retry(&e) {
                    last_error = Some(e);
                    continue;
                }
                req.status.send(Status::Failed).ok();
                req.result.send(Err(e)).ok();
                return;
            }
        }
    }
}

async fn download(client: Client, req: &mut DownloadRequest) -> Result<File, Error> {
    let update_progress = |bytes_downloaded: u64, total_bytes: Option<u64>| {
        req.status
            .send(Status::InProgress(DownloadProgress {
                bytes_downloaded,
                total_bytes,
            }))
            .ok();
    };

    let mut response = client
        .get(req.url.as_ref())
        .send()
        .await?
        .error_for_status()?;
    let total_bytes = response.content_length();
    let mut bytes_downloaded = 0u64;

    // Create the destination directory if it doesn't exist
    if let Some(parent) = req.destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = File::create(&req.destination).await?;

    update_progress(bytes_downloaded, total_bytes);
    loop {
        tokio::select! {
            _ = &mut req.cancel => {
                drop(file); // Manually drop the file handle to ensure that deletion doesn't fail
                tokio::fs::remove_file(&req.destination).await?;
                return Err(Error::Download(DownloadError::Cancelled));
            }
            chunk = response.chunk() => {
                match chunk {
                    Ok(Some(chunk)) => {
                        file.write_all(&chunk).await?;
                        bytes_downloaded += chunk.len() as u64;
                        update_progress(bytes_downloaded, total_bytes);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        drop(file); // Manually drop the file handle to ensure that deletion doesn't fail
                        tokio::fs::remove_file(&req.destination).await?;
                        return Err(Error::Reqwest(e))
                    },
                }
            }
        }
    }
    update_progress(bytes_downloaded, total_bytes);

    // Ensure the data is written to disk
    file.sync_all().await?;
    // Open a new file handle with RO permissions
    let file = File::options().read(true).open(&req.destination).await?;
    Ok(file)
}
