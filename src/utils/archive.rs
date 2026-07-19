use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use flate2::read::GzDecoder;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("archive name is not valid UTF-8: {0}")]
    InvalidName(PathBuf),
    #[error("unsupported archive: {0}")]
    Unsupported(PathBuf),
    #[error("archive entry escaped the staging directory: {0}")]
    EntryOutsideDestination(PathBuf),
    #[error("archives must not contain links: {0}")]
    Link(PathBuf),
}

pub(crate) fn extract(archive: &Path, destination: &Path) -> Result<(), ArchiveError> {
    let name = archive
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ArchiveError::InvalidName(archive.to_path_buf()))?;
    let file = File::open(archive)?;
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        unpack(GzDecoder::new(file), destination)
    } else if name.ends_with(".tar") {
        unpack(file, destination)
    } else {
        Err(ArchiveError::Unsupported(archive.to_path_buf()))
    }
}

fn unpack(reader: impl Read, destination: &Path) -> Result<(), ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            fs::create_dir_all(destination.join(path))?;
        } else if entry_type.is_file() {
            if !entry.unpack_in(destination)? {
                return Err(ArchiveError::EntryOutsideDestination(path));
            }
        } else {
            return Err(ArchiveError::Link(path));
        }
    }
    Ok(())
}

pub(crate) fn files(root: &Path) -> Result<Vec<PathBuf>, ArchiveError> {
    let mut files = Vec::new();
    for entry in sorted_entries(root)? {
        if entry.file_type()?.is_dir() {
            files.extend(self::files(&entry.path())?);
        } else if entry.file_type()?.is_file() {
            files.push(entry.path());
        } else {
            return Err(ArchiveError::Link(entry.path()));
        }
    }
    Ok(files)
}

fn sorted_entries(path: &Path) -> Result<Vec<fs::DirEntry>, ArchiveError> {
    let mut entries = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}
