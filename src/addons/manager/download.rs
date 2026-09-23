//! Shared download progress and cancellation handling.

use crate::{
    Transfer,
    error::{Error, Result},
};
use download_manager::{events::Progress as DownloadProgress, manager::DownloadManager};
use futures_util::{FutureExt, StreamExt};
use std::path::Path;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Drives a download, translating its latest byte counts and cancellation result.
pub(super) async fn download(
    downloader: &DownloadManager,
    url: Url,
    destination: &Path,
    cancellation: &CancellationToken,
    mut on_progress: impl FnMut(Transfer),
) -> Result<()> {
    let download = downloader.download(url, destination)?;
    let mut updates = Box::pin(
        download
            .progress()
            .chain(futures_util::stream::pending::<DownloadProgress>()),
    );
    let result = download.clone().fuse();
    let cancelled = cancellation.cancelled().fuse();
    futures_util::pin_mut!(result, cancelled);

    loop {
        futures_util::select_biased! {
            result = result => {
                result?;
                return Ok(());
            },
            _ = cancelled => {
                download.cancel().await?;
                return Err(Error::Cancelled);
            }
            update = updates.next().fuse() => {
                let update = update.expect("progress stream is chained with pending");
                on_progress(Transfer {
                    current: update.bytes_downloaded(),
                    total: update.total_bytes(),
                });
            }
        }
    }
}
