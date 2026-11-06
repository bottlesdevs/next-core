//! Filesystem queries, archive handling, and disposable workspaces.
//!
//! Temporary-workspace cleanup is best effort: the work result determines
//! success, and panicking or dropping an in-flight future does not schedule cleanup.

pub(crate) mod archive;

use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{error::ResultExt, utils::paths};

/// Creates an isolated child of `parent`, runs `work`, then removes the child.
///
/// The child directory is passed to `work` after successful creation. When
/// `work` returns, cleanup is attempted for both [`Ok`] and [`Err`] results;
/// cleanup errors are logged and do not replace that result. A panic in `work`
/// or dropping this future before completion can leave the directory on disk.
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

/// Moves `path` into the trash directory, then removes it.
///
/// Once the rename succeeds, `path` is gone; removal from the trash is logged
/// rather than returned.
///
/// # Errors
///
/// Returns an error if the trash directory cannot be created or `path` cannot
/// be moved into it.
pub(crate) async fn discard(path: &Path) -> io::Result<()> {
    let trash = paths::trash();
    async_fs::create_dir_all(&trash).await?;
    let destination = trash.join(Uuid::new_v4().to_string());
    async_fs::rename(path, &destination).await?;
    async_fs::remove_dir_all(destination).await.log_warn();
    Ok(())
}

/// Tests whether `path` exists without masking errors other than not-found.
///
/// Metadata lookup follows symbolic links, so a dangling symlink returns `false`.
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
