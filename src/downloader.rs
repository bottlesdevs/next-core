use reqwest::{Client, Url};
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};

use crate::Error;

const QUEUE_SIZE: usize = 100;

#[derive(Debug)]
struct DownloadRequest {
    url: Url,
    destination: PathBuf,
    result: oneshot::Sender<Result<File, Error>>,
    status: watch::Sender<Status>,
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
}

impl DownloadHandle {
    pub async fn r#await(self) -> Result<std::fs::File, Error> {
        match self.result.await {
            Ok(result) => result,
            Err(_) => todo!(),
        }
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Status {
    Pending,
    InProgress(DownloadProgress),
    Completed,
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

        let req = DownloadRequest {
            url,
            destination,
            result: result_tx,
            status: status_tx,
        };

        let _ = self.queue.try_send(req);

        DownloadHandle {
            result: result_rx,
            status: status_rx,
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
            match download_thread(client, &request).await {
                Ok(file) => {
                    let _ = request.status.send(Status::Completed);
                    let _ = request.result.send(Ok(file));
                }
                Err(e) => {
                    let _ = request.status.send(Status::Failed);
                    let _ = request.result.send(Err(e));
                }
            }
        });
    }
}

async fn download_thread(client: Client, req: &DownloadRequest) -> Result<File, Error> {
    let mut resp = client.get(req.url.as_ref()).send().await?;
    let total_bytes = resp.content_length();
    let mut bytes_downloaded = 0u64;
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .open(&req.destination)?;

    while let Some(chunk) = resp.chunk().await.transpose() {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        bytes_downloaded += chunk.len() as u64;
        let _ = req.status.send(Status::InProgress(DownloadProgress {
            bytes_downloaded,
            total_bytes,
        }));
    }

    // Reset the cursor to the beginning of the file
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}
