//! Disposable UUID workspaces under the configured staging and trash directories.
//! Rename publishes or withdraws; cleanup never decides success. No startup sweep or recovery.

use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::error::ResultExt;

/// Create an isolated directory and its parent, then clean up after the returned result.
/// Dropping the future performs no asynchronous cleanup.
pub(crate) async fn with_temp_dir<T, E, F>(
    parent: &Path,
    work: impl FnOnce(PathBuf) -> F,
) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
    E: From<io::Error>,
{
    let directory = parent.join(Uuid::new_v4().to_string());
    async_fs::create_dir_all(&directory).await?;
    let result = work(directory.clone()).await;
    async_fs::remove_dir_all(directory).await.log_warn();
    result
}
