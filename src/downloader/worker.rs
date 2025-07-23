use super::{DownloadProgress, DownloadRequest};
use crate::{downloader::Status, error::DownloadError, Error};
use reqwest::Client;
use std::time::Duration;
use tokio::{fs::File, io::AsyncWriteExt};

const MAX_RETRIES: usize = 3;

pub(super) async fn download_thread(client: Client, mut req: DownloadRequest) {
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
                        .map(ToString::to_string)
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
                _ = req.cancel.cancelled() => {
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

                let status = if matches!(e, Error::Download(DownloadError::Cancelled)) {
                    Status::Cancelled
                } else {
                    Status::Failed
                };
                req.status.send(status).ok();
                req.result.send(Err(e)).ok();
                return;
            }
        }
    }
}

async fn download(client: Client, req: &mut DownloadRequest) -> Result<File, Error> {
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

    let update_interval = Duration::from_millis(250);
    let mut progress = DownloadProgress::new(bytes_downloaded, total_bytes, update_interval);
    req.status.send(Status::InProgress(progress)).ok();

    loop {
        tokio::select! {
            _ = req.cancel.cancelled() => {
                drop(file); // Manually drop the file handle to ensure that deletion doesn't fail
                tokio::fs::remove_file(&req.destination).await?;
                return Err(Error::Download(DownloadError::Cancelled));
            }
            chunk = response.chunk() => {
                match chunk {
                    Ok(Some(chunk)) => {
                        file.write_all(&chunk).await?;
                        bytes_downloaded += chunk.len() as u64;
                        if let Some(new_progress) = progress.update(bytes_downloaded) {
                            progress = new_progress;
                        }
                        req.status.send(Status::InProgress(progress)).ok();
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

    // Ensure the data is written to disk
    file.sync_all().await?;
    // Open a new file handle with RO permissions
    let file = File::options().read(true).open(&req.destination).await?;
    Ok(file)
}
