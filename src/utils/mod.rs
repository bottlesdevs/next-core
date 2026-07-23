pub(crate) mod archive;
pub(crate) mod context;
pub(crate) mod directories;
pub(crate) mod environment;

use crate::error::Result;
use std::path::PathBuf;

pub fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };

    Ok(path.components().collect())
}
