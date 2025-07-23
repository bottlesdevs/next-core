use super::{config::DownloadManagerConfig, download_thread, DownloadHandle, DownloadRequest};
use crate::{error::DownloadError, Error};
use reqwest::{Client, Url};
use std::{path::Path, sync::Arc};
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct DownloadManager {
    queue: mpsc::Sender<DownloadRequest>,
    semaphore: Arc<Semaphore>,
    cancel: CancellationToken,
    config: DownloadManagerConfig,
}

impl Drop for DownloadManager {
    fn drop(&mut self) {
        // Need to manually close the semaphore to make sure dispatcher_thread stops waiting for permits
        self.semaphore.close();
    }
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::with_config(DownloadManagerConfig::default())
    }
}

impl DownloadManager {
    pub fn with_config(config: DownloadManagerConfig) -> Self {
        let (tx, rx) = mpsc::channel(config.queue_size());
        let client = Client::new();
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent()));
        let manager = Self {
            queue: tx,
            semaphore: semaphore.clone(),
            cancel: CancellationToken::new(),
            config,
        };
        // Spawn the dispatcher thread to handle download requests
        tokio::spawn(async move { dispatcher_thread(client, rx, semaphore).await });
        manager
    }

    pub fn download(
        &self,
        url: Url,
        destination: impl AsRef<Path>,
    ) -> Result<DownloadHandle, Error> {
        if self.cancel.is_cancelled() {
            return Err(Error::Download(DownloadError::ManagerShutdown));
        }

        let destination = destination.as_ref();
        if destination.exists() {
            return Err(Error::Download(DownloadError::FileExists {
                path: destination.to_path_buf(),
            }));
        }

        let cancel = self.cancel.child_token();
        let (req, handle) = DownloadRequest::new_req_handle_pair(url, destination, cancel);

        self.queue.try_send(req).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => Error::Download(DownloadError::QueueFull),
            mpsc::error::TrySendError::Closed(_) => Error::Download(DownloadError::ManagerShutdown),
        })?;

        Ok(handle)
    }

    pub async fn set_max_parallel_downloads(&self, limit: usize) -> Result<(), Error> {
        let current = self.config.max_concurrent();
        if limit > current {
            self.semaphore.add_permits(limit - current);
        } else if limit < current {
            let to_remove = current - limit;

            let permits = self
                .semaphore
                .acquire_many(to_remove as u32)
                .await
                .map_err(|_| Error::Download(DownloadError::ManagerShutdown))?;

            permits.forget();
        }
        self.config.set_max_concurrent(limit);

        Ok(())
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
