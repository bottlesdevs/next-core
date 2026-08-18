pub(crate) mod archive;
pub(crate) mod checksum;
pub(crate) mod context;
pub(crate) mod directories;
pub(crate) mod environment;

#[cfg(feature = "fvs")]
use std::path::PathBuf;
use std::{io, path::Path};

#[cfg(feature = "fvs")]
pub fn absolute_path(path: PathBuf) -> crate::error::Result<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };

    Ok(path.components().collect())
}

/// Searches `$PATH` for an executable named `name`, the same way a shell
/// would resolve a bare command. Returns the first match, or `None` if
/// `$PATH` is unset or nothing on it matches.
#[cfg(feature = "fvs")]
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(name);
        is_executable_file(&candidate).then_some(candidate)
    })
}

#[cfg(feature = "fvs")]
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(crate) async fn exists(path: impl AsRef<Path>) -> io::Result<bool> {
    match async_fs::metadata(path).await {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
