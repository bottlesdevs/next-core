//! Download backend abstraction and implementations
//!
//! This module provides a trait-based abstraction for download backends,
//! allowing different HTTP clients or protocols to be used. It includes
//! a default Reqwest-based implementation with HTTP Range support for
//! resumable downloads.

use crate::download::types::{DownloadTask, ProgressUpdate};
use crate::Error;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use reqwest::header::{self, HeaderMap, HeaderValue};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

/// Abstraction for download backends
///
/// Implement this trait to provide custom download functionality.
/// The backend handles the actual HTTP requests and response streaming,
/// while the manager handles queuing, retries, and state management.
#[async_trait]
pub trait DownloadBackend: Send + Sync + std::fmt::Debug {
    /// Download a file with progress reporting
    ///
    /// This method should download the file at `task.url` to `task.temp_path`,
    /// streaming the data in chunks and sending progress updates via the channel.
    /// It should support resume from partial downloads if `resume_from` is provided.
    ///
    /// # Arguments
    ///
    /// * `task` - The download task containing URL, paths, and metadata
    /// * `resume_from` - Number of bytes already downloaded (for resume)
    /// * `progress_tx` - Channel sender for progress updates
    ///
    /// # Returns
    ///
    /// Returns the total number of bytes downloaded, or an error if the download failed.
    ///
    /// # Cancel Safety
    ///
    /// This method should be cancel-safe - if the returned future is dropped,
    /// the partial download should remain intact for resuming.
    async fn download(
        &self,
        task: &DownloadTask,
        resume_from: u64,
        progress_tx: mpsc::UnboundedSender<ProgressUpdate>,
    ) -> Result<u64, Error>;

    /// Check if the server supports range requests
    ///
    /// This is called before attempting to resume a download to verify
    /// that the server supports partial content requests.
    ///
    /// # Arguments
    ///
    /// * `task` - The download task to check
    ///
    /// # Returns
    ///
    /// Returns `true` if the server supports range requests, `false` otherwise.
    async fn supports_resume(&self, task: &DownloadTask) -> Result<bool, Error>;

    /// Get the content length of a URL without downloading
    ///
    /// # Arguments
    ///
    /// * `url` - The URL to check
    ///
    /// # Returns
    ///
    /// Returns the content length in bytes, or `None` if not available.
    async fn get_content_length(&self, url: &url::Url) -> Result<Option<u64>, Error>;
}

/// Reqwest-based download backend
///
/// The default implementation using the `reqwest` HTTP client.
/// Supports HTTP/HTTPS, resume via Range headers, and custom headers.
#[derive(Debug, Clone)]
pub struct ReqwestBackend {
    client: reqwest::Client,
}

impl ReqwestBackend {
    /// Create a new ReqwestBackend with default configuration
    pub fn new() -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()?;

        Ok(Self { client })
    }

    /// Create a new ReqwestBackend with a custom client
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Build headers for a download request
    fn build_headers(&self, task: &DownloadTask, resume_from: u64) -> HeaderMap {
        let mut headers = HeaderMap::new();

        // Add custom headers from task
        if let Some(custom_headers) = &task.headers {
            for (key, value) in custom_headers {
                if let Ok(header_name) = key.parse::<header::HeaderName>() {
                    if let Ok(header_value) = HeaderValue::from_str(value) {
                        headers.insert(header_name, header_value);
                    }
                }
            }
        }

        // Add Range header for resume
        if resume_from > 0 {
            let range_value = format!("bytes={}-", resume_from);
            if let Ok(value) = HeaderValue::from_str(&range_value) {
                headers.insert(header::RANGE, value);
            }
        }

        headers
    }
}

impl Default for ReqwestBackend {
    fn default() -> Self {
        Self::new().expect("Failed to create default ReqwestBackend")
    }
}

#[async_trait]
impl DownloadBackend for ReqwestBackend {
    async fn download(
        &self,
        task: &DownloadTask,
        resume_from: u64,
        progress_tx: mpsc::UnboundedSender<ProgressUpdate>,
    ) -> Result<u64, Error> {
        let headers = self.build_headers(task, resume_from);

        let request = self
            .client
            .get(task.url.as_str())
            .headers(headers)
            .build()
            .map_err(|e| Error::Download(format!("Failed to build request: {}", e)))?;

        let response = self
            .client
            .execute(request)
            .await
            .map_err(|e| Error::Http(e))?;

        // Check for successful status
        let status = response.status();
        if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(Error::Download(format!(
                "HTTP error {}: {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown")
            )));
        }

        // Get total size from headers
        let total_bytes = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        // If resuming, add the resume offset to get total
        let total_bytes = if resume_from > 0 {
            total_bytes.map(|t| t + resume_from)
        } else {
            total_bytes
        };

        // Create or open the temp file for writing
        let file_mode = if resume_from > 0 {
            tokio::fs::OpenOptions::new()
                .write(true)
                .append(true)
                .open(&task.temp_path)
                .await
        } else {
            tokio::fs::File::create(&task.temp_path).await
        };

        let mut file = file_mode.map_err(|e| Error::Io(e))?;

        // Stream the response body
        let mut stream = response.bytes_stream();
        let mut bytes_downloaded = resume_from;
        let mut last_progress_time = std::time::Instant::now();
        let mut last_progress_bytes = resume_from;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| Error::Http(e))?;
            
            // Write chunk to file
            file.write_all(&chunk).await.map_err(|e| Error::Io(e))?;
            
            // Update progress
            bytes_downloaded += chunk.len() as u64;
            
            // Calculate speed every 100ms or on every chunk if small
            let now = std::time::Instant::now();
            if now.duration_since(last_progress_time).as_millis() >= 100 || chunk.len() < 1024 {
                let elapsed = now.duration_since(last_progress_time);
                let bytes_diff = bytes_downloaded - last_progress_bytes;
                let speed = if elapsed.as_secs_f64() > 0.0 {
                    bytes_diff as f64 / elapsed.as_secs_f64()
                } else {
                    0.0
                };

                let mut progress = ProgressUpdate::new(bytes_downloaded, total_bytes);
                progress.bytes_per_second = speed;
                
                if let Some(total) = total_bytes {
                    let remaining = total.saturating_sub(bytes_downloaded);
                    if speed > 0.0 {
                        progress.eta_seconds = Some((remaining as f64 / speed) as u64);
                    }
                }

                // Send progress update (ignore send errors)
                let _ = progress_tx.send(progress);
                
                last_progress_time = now;
                last_progress_bytes = bytes_downloaded;
            }
        }

        // Flush and sync file to ensure data is written
        file.flush().await.map_err(|e| Error::Io(e))?;
        file.sync_all().await.map_err(|e| Error::Io(e))?;

        // Send final progress update
        let mut final_progress = ProgressUpdate::new(bytes_downloaded, total_bytes);
        final_progress.bytes_per_second = 0.0;
        final_progress.eta_seconds = Some(0);
        let _ = progress_tx.send(final_progress);

        Ok(bytes_downloaded)
    }

    async fn supports_resume(&self, task: &DownloadTask) -> Result<bool, Error> {
        // Send a HEAD request to check for Accept-Ranges header
        let response = self
            .client
            .head(task.url.as_str())
            .send()
            .await
            .map_err(|e| Error::Http(e))?;

        // Check for Accept-Ranges: bytes header
        let accept_ranges = response
            .headers()
            .get(header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok());

        Ok(matches!(accept_ranges, Some("bytes")))
    }

    async fn get_content_length(&self, url: &url::Url) -> Result<Option<u64>, Error> {
        let response = self
            .client
            .head(url.as_str())
            .send()
            .await
            .map_err(|e| Error::Http(e))?;

        let content_length = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        Ok(content_length)
    }
}

/// Type alias for a boxed download backend
pub type BoxedBackend = Box<dyn DownloadBackend>;

/// Stream wrapper that can be cancelled
///
/// Wraps a byte stream and checks for cancellation signals between chunks.
pub struct CancellableStream<S> {
    inner: S,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<S> CancellableStream<S> {
    pub fn new(inner: S, cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self { inner, cancelled }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl<S, E> Stream for CancellableStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<Bytes, E>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.is_cancelled() {
            return Poll::Ready(None);
        }
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::types::{DownloadConfig, TaskState};
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_reqwest_backend_creation() {
        let backend = ReqwestBackend::new();
        assert!(backend.is_ok());
    }

    #[tokio::test]
    async fn test_get_content_length() {
        let _backend = ReqwestBackend::new().unwrap();
        // This test would need a mock server in practice
        // For now, just verify the method exists and compiles
        let _url = url::Url::parse("https://dummyimage.com/6000x4000/000/fff.png&text=test").unwrap();
        // We won't actually call this as it would make a network request
        // In real tests, use a mock server like wiremock
    }

    #[test]
    fn test_build_headers() {
        let backend = ReqwestBackend::default();
        let config = DownloadConfig::default();
        let task = DownloadTask::new(
            url::Url::parse("https://dummyimage.com/6000x4000/000/fff.png&text=test").unwrap(),
            PathBuf::from("/tmp/file.txt"),
            &config,
        );

        let headers = backend.build_headers(&task, 0);
        assert!(!headers.contains_key(header::RANGE));

        let headers_resume = backend.build_headers(&task, 1024);
        assert!(headers_resume.contains_key(header::RANGE));
        assert_eq!(
            headers_resume.get(header::RANGE).unwrap().to_str().unwrap(),
            "bytes=1024-"
        );
    }
}
