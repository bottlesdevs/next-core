use reqwest::{Client, Url};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};
use tokio_util::sync::CancellationToken;

use super::{download_thread, DownloadHandle, DownloadRequest, Status};

const QUEUE_SIZE: usize = 100;

#[derive(Debug)]
pub struct DownloadManager {
    queue: mpsc::Sender<DownloadRequest>,
    semaphore: Arc<Semaphore>,
    cancel: CancellationToken,
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
            cancel: CancellationToken::new(),
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
        let (status_tx, status_rx) = watch::channel(Status::Queued);
        let cancel = self.cancel.child_token();

        let req = DownloadRequest {
            url,
            destination,
            result: result_tx,
            status: status_tx,
            cancel: cancel.clone(),
        };

        let _ = self.queue.try_send(req);

        DownloadHandle::new(result_rx, status_rx, cancel)
    }

    pub fn cancel_all(&self) {
        self.cancel.cancel();
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
