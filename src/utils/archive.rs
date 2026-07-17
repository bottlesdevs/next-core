use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use flate2::read::GzDecoder;

pub(crate) fn extract(archive: &Path, destination: &Path) -> io::Result<()> {
    let name = archive
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_data("archive name is not valid UTF-8"))?;
    let file = File::open(archive)?;
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        unpack(GzDecoder::new(file), destination)
    } else if name.ends_with(".tar") {
        unpack(file, destination)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("unsupported archive: {}", archive.display()),
        ))
    }
}

fn unpack(reader: impl Read, destination: &Path) -> io::Result<()> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            fs::create_dir_all(destination.join(path))?;
        } else if entry_type.is_file() {
            if !entry.unpack_in(destination)? {
                return Err(invalid_data("archive entry escaped the staging directory"));
            }
        } else {
            return Err(invalid_data("archives must not contain links"));
        }
    }
    Ok(())
}

pub(crate) fn files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in sorted_entries(root)? {
        if entry.file_type()?.is_dir() {
            files.extend(self::files(&entry.path())?);
        } else if entry.file_type()?.is_file() {
            files.push(entry.path());
        } else {
            return Err(invalid_data("staged archive contains a link"));
        }
    }
    Ok(files)
}

fn sorted_entries(path: &Path) -> io::Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
