//! Disposable storage. Rename publishes or withdraws; cleanup never decides success.

use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::error::{Result, ResultExt};

async fn create_stage(staging: &Path) -> io::Result<PathBuf> {
    let path = staging.join(Uuid::new_v4().to_string());
    async_fs::create_dir_all(&path).await?;
    Ok(path)
}

/// Clean up after a returned result, including cooperative cancellation.
/// Dropping the future performs no asynchronous cleanup.
pub(crate) async fn with_stage<T, F>(staging: &Path, work: impl FnOnce(PathBuf) -> F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let stage = create_stage(staging).await?;
    let result = work(stage.clone()).await;
    cleanup(&stage).await;
    result
}

/// Remove a disposable file or directory without following symlinks.
pub(crate) async fn cleanup(path: &Path) {
    async {
        if async_fs::symlink_metadata(path).await?.is_dir() {
            async_fs::remove_dir_all(path).await
        } else {
            async_fs::remove_file(path).await
        }
    }
    .await
    .log_warn();
}
