//! Filesystem queries, archive handling, and disposable workspaces.
//!
//! Temporary-workspace cleanup is best effort: the work result determines
//! success, and dropping an in-flight future does not schedule cleanup.

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
/// The child directory is passed to `work` after successful creation. Cleanup is
/// attempted whether `work` succeeds or fails; cleanup errors are logged and do
/// not replace its result. Dropping this future before completion can leave the
/// directory and its contents on disk.
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
/// Converts `path` to an absolute path without accessing the target.
///
/// Relative paths are joined to the process's current directory. Collecting the
/// path components removes redundant separators and current-directory components,
/// but preserves parent-directory components; symbolic links are not resolved.
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
