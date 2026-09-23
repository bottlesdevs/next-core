//! Filesystem queries, archive handling, and disposable workspaces.
//!
//! Temporary workspaces use random UUID names. Their cleanup is best effort:
//! the work result determines success, and dropping an in-flight future does
//! not schedule asynchronous cleanup.

pub(crate) mod archive;

use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::error::ResultExt;

/// Creates an isolated child of `parent`, runs `work`, then removes the child.
///
/// Cleanup errors are logged and do not replace the result of `work`. Dropping
/// this future before completion does not perform asynchronous cleanup.
///
/// # Errors
///
/// Returns an error if the workspace cannot be created or if `work` returns an
/// error. Removal failures are not returned.
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

#[cfg(feature = "fvs")]
/// Converts `path` to a lexically normalized absolute path.
///
/// Relative paths are resolved against the process's current directory. This
/// does not access the filesystem or resolve symbolic links.
///
/// # Errors
///
/// Returns an I/O error if the current directory cannot be read.
pub fn absolute_path(path: PathBuf) -> crate::error::Result<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };

    Ok(path.components().collect())
}

/// Tests whether `path` exists without masking errors other than not-found.
///
/// # Errors
///
/// Returns metadata errors other than [`io::ErrorKind::NotFound`].
pub(crate) async fn exists(path: impl AsRef<Path>) -> io::Result<bool> {
    match async_fs::metadata(path).await {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
