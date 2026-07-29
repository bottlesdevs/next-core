use std::{
    io,
    path::{Component, Path, PathBuf},
};

use async_compression::futures::bufread::GzipDecoder;
use futures_lite::{
    StreamExt,
    io::{AsyncRead, BufReader, copy},
};
use smol_tar::{TarEntry, TarReader};
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
    #[error("unsupported archive entry: {0}")]
    UnsupportedEntry(PathBuf),
}

pub(crate) async fn extract(archive: &Path, destination: &Path) -> Result<(), ArchiveError> {
    let name = archive
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ArchiveError::InvalidName(archive.to_path_buf()))?;
    let file = async_fs::File::open(archive).await?;
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        unpack(GzipDecoder::new(BufReader::new(file)), destination).await
    } else if name.ends_with(".tar") {
        unpack(file, destination).await
    } else {
        Err(ArchiveError::Unsupported(archive.to_path_buf()))
    }
}

async fn unpack(
    reader: impl AsyncRead + Send + 'static,
    destination: &Path,
) -> Result<(), ArchiveError> {
    let mut archive = TarReader::new(reader);
    let mut directories = Vec::new();

    while let Some(entry) = archive.next().await {
        match entry? {
            TarEntry::File(mut file) => {
                let path = safe_path(file.path())?;
                let destination = destination.join(&path);
                if let Some(parent) = destination.parent() {
                    async_fs::create_dir_all(parent).await?;
                }
                let mut output = async_fs::File::create(&destination).await?;
                copy(&mut file, &mut output).await?;
                set_mode(&destination, file.mode()).await?;
            }
            TarEntry::Directory(directory) => {
                let path = safe_path(directory.path())?;
                let destination = destination.join(path);
                async_fs::create_dir_all(&destination).await?;
                directories.push((destination, directory.mode()));
            }
            entry => {
                return Err(ArchiveError::UnsupportedEntry(PathBuf::from(entry.path())));
            }
        }
    }

    for (path, mode) in directories.into_iter().rev() {
        set_mode(&path, mode).await?;
    }
    Ok(())
}

fn safe_path(path: &str) -> Result<PathBuf, ArchiveError> {
    let path = Path::new(path);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(ArchiveError::EntryOutsideDestination(path.to_path_buf()));
    }
    Ok(path.to_path_buf())
}

#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    async_fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await
}

#[cfg(not(unix))]
async fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

pub(crate) async fn files(root: &Path) -> Result<Vec<PathBuf>, ArchiveError> {
    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(directory) = directories.pop() {
        let mut entries = async_fs::read_dir(directory).await?;
        while let Some(entry) = entries.next().await {
            let entry = entry?;
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            } else {
                return Err(ArchiveError::UnsupportedEntry(entry.path()));
            }
        }
    }

    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use async_compression::futures::write::GzipEncoder;
    use futures_lite::io::AsyncWriteExt;
    use smol_tar::{TarRegularFile, TarSymlink, TarWriter};

    use super::*;

    fn temporary_directory() -> PathBuf {
        std::env::temp_dir().join(format!("bottles-next-archive-{}", uuid::Uuid::new_v4()))
    }

    async fn tar_with_file(path: &str, mode: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        let body = b"#!/bin/sh\n";
        {
            let mut archive = TarWriter::new(&mut bytes);
            archive
                .write(
                    TarRegularFile::new(path, body.len() as u64, body.as_slice())
                        .with_mode(mode)
                        .into(),
                )
                .await
                .unwrap();
            archive.finish().await.unwrap();
        }
        bytes
    }

    #[test]
    fn extracts_gzip_and_preserves_file_mode() {
        futures_lite::future::block_on(async {
            let root = temporary_directory();
            let archive_path = root.join("component.tgz");
            let destination = root.join("output");
            async_fs::create_dir_all(&destination).await.unwrap();
            let mut archive =
                GzipEncoder::new(async_fs::File::create(&archive_path).await.unwrap());
            archive
                .write_all(&tar_with_file("component/run.sh", 0o755).await)
                .await
                .unwrap();
            archive.close().await.unwrap();

            extract(&archive_path, &destination).await.unwrap();
            assert_eq!(
                async_fs::read(destination.join("component/run.sh"))
                    .await
                    .unwrap(),
                b"#!/bin/sh\n"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                assert_eq!(
                    async_fs::metadata(destination.join("component/run.sh"))
                        .await
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o755
                );
            }

            async_fs::remove_dir_all(root).await.unwrap();
        });
    }

    #[test]
    fn rejects_paths_outside_destination() {
        futures_lite::future::block_on(async {
            let root = temporary_directory();
            let archive_path = root.join("component.tar");
            let destination = root.join("output");
            async_fs::create_dir_all(&destination).await.unwrap();
            async_fs::write(&archive_path, tar_with_file("../escaped", 0o644).await)
                .await
                .unwrap();

            assert!(matches!(
                extract(&archive_path, &destination).await,
                Err(ArchiveError::EntryOutsideDestination(_))
            ));
            assert!(!root.join("escaped").exists());

            async_fs::remove_dir_all(root).await.unwrap();
        });
    }

    #[test]
    fn rejects_links() {
        futures_lite::future::block_on(async {
            let root = temporary_directory();
            let archive_path = root.join("component.tar");
            let destination = root.join("output");
            async_fs::create_dir_all(&destination).await.unwrap();
            let mut bytes = Vec::new();
            {
                let mut archive = TarWriter::<_, &[u8]>::new(&mut bytes);
                archive
                    .write(TarSymlink::new("component/link", "target").into())
                    .await
                    .unwrap();
                archive.finish().await.unwrap();
            }
            async_fs::write(&archive_path, bytes).await.unwrap();

            assert!(matches!(
                extract(&archive_path, &destination).await,
                Err(ArchiveError::UnsupportedEntry(_))
            ));

            async_fs::remove_dir_all(root).await.unwrap();
        });
    }
}
