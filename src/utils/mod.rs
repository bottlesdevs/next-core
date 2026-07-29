pub(crate) mod archive;
pub(crate) mod context;
pub(crate) mod directories;
pub(crate) mod environment;

use std::{
    io,
    path::{Path, PathBuf},
};

pub fn absolute_path(path: PathBuf) -> crate::error::Result<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };

    Ok(path.components().collect())
}

pub(crate) async fn exists(path: impl AsRef<Path>) -> io::Result<bool> {
    match async_fs::metadata(path).await {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
