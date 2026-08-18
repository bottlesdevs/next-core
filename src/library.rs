//! Local install-state persistence and chunked-download orchestration for
//! game library installs.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use download_manager::{download::Download, manager::DownloadManager};
use next_config::Config;
use next_proto::bottles::common::v1::{InstallState, Storefront};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use tokio_stream::StreamExt;

use crate::{Bottle, bottle::error::BottleError, error::Result};

/// Library/install-specific failures carried by [`crate::error::Error::Library`].
#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("bad chunk URL for {file}: {source}")]
    InvalidChunkUrl {
        file: String,
        source: url::ParseError,
    },
    #[error("failed to enqueue a chunk of {file}: {source}")]
    EnqueueFailed {
        file: String,
        source: download_manager::error::Error,
    },
    #[error("{file} ({url}): {source}")]
    ChunkDownloadFailed {
        file: String,
        url: String,
        source: download_manager::error::Error,
    },
}

const INSTALLS_FILE: &str = "installs.toml";

#[derive(Debug, Default, Clone, Serialize, Deserialize, Config)]
#[config(version = 1)]
struct InstallsConfig {
    installs: Vec<InstallRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    pub profile_id: String,
    pub storefront: i32,
    pub game_id: String,
    pub version: String,
    pub install_size_bytes: Option<u64>,
    /// Which bottle's `C:` drive these files were written into.
    pub bottle_id: String,
    /// Paths installed relative to that bottle's `C:` drive, recorded
    /// at install time so uninstalling removes exactly what was
    /// written even if the manifest changes later.
    pub relative_paths: Vec<String>,
    /// The `Program` registered on the bottle for this install's launch
    /// executable, if one was found — so uninstalling can remove it
    /// too. Unset when no primary executable could be determined.
    pub program_id: Option<String>,
}

impl InstallRecord {
    fn matches(&self, profile_id: &str, storefront: Storefront, game_id: &str) -> bool {
        self.profile_id == profile_id
            && self.storefront == storefront as i32
            && self.game_id == game_id
    }

    pub fn install_state(&self) -> InstallState {
        InstallState {
            installed: true,
            bottle_id: Some(self.bottle_id.clone()),
            installed_version: Some(self.version.clone()),
            install_size_bytes: self.install_size_bytes,
        }
    }
}

fn installs_path() -> Result<PathBuf> {
    directories::ProjectDirs::from("com", "usebottles", "bottles-next")
        .map(|dirs| dirs.config_dir().join(INSTALLS_FILE))
        .ok_or_else(|| BottleError::ProjectDirectoriesUnavailable.into())
}

/// Where a game's files are written. Not part of `InstallsConfig` itself
/// since it's derived, not persisted.
pub fn install_dir(profile_id: &str, storefront: Storefront, game_id: &str) -> Result<PathBuf> {
    directories::ProjectDirs::from("com", "usebottles", "bottles-next")
        .map(|dirs| {
            dirs.data_dir()
                .join("installs")
                .join(profile_id)
                .join(storefront.as_str_name())
                .join(game_id)
        })
        .ok_or_else(|| BottleError::ProjectDirectoriesUnavailable.into())
}

pub struct InstallsStore {
    path: PathBuf,
    state: RwLock<InstallsConfig>,
}

impl InstallsStore {
    pub async fn load() -> Result<Self> {
        let path = installs_path()?;
        let state = match next_config::load::<InstallsConfig>(&path).await {
            Ok(state) => state,
            Err(next_config::error::Error::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                InstallsConfig::default()
            }
            Err(err) => return Err(err.into()),
        };
        Ok(Self {
            path,
            state: RwLock::new(state),
        })
    }

    pub async fn get(
        &self,
        profile_id: &str,
        storefront: Storefront,
        game_id: &str,
    ) -> Option<InstallRecord> {
        self.state
            .read()
            .await
            .installs
            .iter()
            .find(|record| record.matches(profile_id, storefront, game_id))
            .cloned()
    }

    async fn persist(&self, state: &InstallsConfig) -> Result<()> {
        next_config::save(&self.path, state).await?;
        Ok(())
    }

    pub async fn upsert(&self, record: InstallRecord) -> Result<()> {
        let mut state = self.state.write().await;
        state.installs.retain(|existing| {
            !existing.matches(&record.profile_id, storefront_of(&record), &record.game_id)
        });
        state.installs.push(record);
        self.persist(&state).await
    }

    /// Removes and returns the record, if any. Callers still need to
    /// delete `install_dir(...)` themselves — this only updates the
    /// record.
    pub async fn remove(
        &self,
        profile_id: &str,
        storefront: Storefront,
        game_id: &str,
    ) -> Result<Option<InstallRecord>> {
        let mut state = self.state.write().await;
        let index = state
            .installs
            .iter()
            .position(|record| record.matches(profile_id, storefront, game_id));
        let Some(index) = index else {
            return Ok(None);
        };
        let record = state.installs.remove(index);
        self.persist(&state).await?;
        Ok(Some(record))
    }
}

fn storefront_of(record: &InstallRecord) -> Storefront {
    Storefront::try_from(record.storefront).unwrap_or(Storefront::Unspecified)
}

/// A single chunk of a file, already resolved to a downloadable URL.
/// Storefronts split files into independently downloaded (and sometimes
/// independently compressed) chunks.
#[derive(Debug, Clone)]
pub struct InstallChunk {
    pub download_url: String,
    pub compressed: bool,
}

/// A file to be written under `install_root`, made of one or more chunks
/// concatenated in order.
#[derive(Debug, Clone)]
pub struct InstallFile {
    pub relative_path: String,
    pub chunks: Vec<InstallChunk>,
}

/// Identifies one in-flight download for [`InstallManager::cancel`] to
/// find. Opaque to `InstallManager` itself — callers choose the key
/// (typically `(profile_id, storefront, game_id)`).
pub type InstallKey = (String, i32, String);

#[derive(Debug, Clone)]
pub struct InstallProgress {
    pub current_file: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_second: f64,
}

#[derive(Debug, Clone)]
pub enum InstallEvent {
    Progress(InstallProgress),
    /// Every file has been downloaded and written under `install_root`.
    /// `relative_paths` are relative to `install_root`'s parent (i.e.
    /// they include `install_root`'s own final path component), matching
    /// what [`InstallRecord::relative_paths`] expects.
    Done {
        relative_paths: Vec<String>,
        install_size_bytes: u64,
    },
}

/// Downloads a flat list of chunked files into a destination directory,
/// reassembling each file from its chunks (decompressing per-chunk where
/// the caller says it's needed). Keeps enough in-flight state to let a
/// separate [`Self::cancel`] call find and cancel a still-running
/// download by key.
pub struct InstallManager {
    downloads: Arc<DownloadManager>,
    installs: Arc<InstallsStore>,
    active: Mutex<HashMap<InstallKey, Vec<Download>>>,
}

impl InstallManager {
    pub fn new(downloads: Arc<DownloadManager>, installs: Arc<InstallsStore>) -> Self {
        Self {
            downloads,
            installs,
            active: Mutex::new(HashMap::new()),
        }
    }

    pub async fn get(
        &self,
        profile_id: &str,
        storefront: Storefront,
        game_id: &str,
    ) -> Option<InstallRecord> {
        self.installs.get(profile_id, storefront, game_id).await
    }

    /// Persists `record`, replacing any existing record for the same
    /// (profile, storefront, game).
    pub async fn record(&self, record: InstallRecord) -> Result<()> {
        self.installs.upsert(record).await
    }

    /// Removes exactly the files a prior install wrote (from its
    /// [`InstallRecord`], not a directory sweep — the bottle's `C:`
    /// drive is shared with every other game installed there) and the
    /// registered launch `Program`, if any. A no-op if no such install
    /// is on record.
    pub async fn uninstall(
        &self,
        profile_id: &str,
        storefront: Storefront,
        game_id: &str,
        bottle: Option<Bottle>,
    ) -> Result<()> {
        let Some(record) = self
            .installs
            .remove(profile_id, storefront, game_id)
            .await?
        else {
            return Ok(());
        };

        if let Some(bottle) = bottle {
            let c_drive = bottle.c_drive_path();
            for relative_path in &record.relative_paths {
                let _ = tokio::fs::remove_file(c_drive.join(relative_path)).await;
            }
            if let Some(program_id) = record.program_id.as_deref()
                && let Ok(program_uuid) = uuid::Uuid::parse_str(program_id)
            {
                let mut edit = bottle.edit();
                edit.remove_program(program_uuid);
                if let Err(err) = edit.commit().await {
                    tracing::warn!("failed to remove launch program for {game_id}: {err}");
                }
            }
        }

        Ok(())
    }

    /// Downloads and writes every file in `files` under
    /// `destination_root`, reporting progress on the returned stream,
    /// which ends with one `Done` event (or an error, if the download
    /// failed or was cancelled via [`Self::cancel`]).
    ///
    /// `install_root_name` is the final path component recorded in
    /// `Done`'s `relative_paths` (e.g. `"Program Files/My Game"`) — kept
    /// separate from `destination_root` since callers persist paths
    /// relative to a shared drive, not the absolute filesystem location.
    pub fn download(
        self: &Arc<Self>,
        key: InstallKey,
        destination_root: PathBuf,
        install_root_name: String,
        staging_dir: PathBuf,
        files: Vec<InstallFile>,
    ) -> impl tokio_stream::Stream<Item = Result<InstallEvent>> + Send + 'static {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let manager = self.clone();

        tokio::spawn(async move {
            if let Err(err) = manager
                .run_download(
                    key,
                    destination_root,
                    install_root_name,
                    staging_dir,
                    files,
                    tx.clone(),
                )
                .await
            {
                let _ = tx.send(Err(err)).await;
            }
        });

        tokio_stream::wrappers::ReceiverStream::new(rx)
    }

    async fn run_download(
        self: &Arc<Self>,
        key: InstallKey,
        destination_root: PathBuf,
        install_root_name: String,
        staging_dir: PathBuf,
        files: Vec<InstallFile>,
        tx: tokio::sync::mpsc::Sender<Result<InstallEvent>>,
    ) -> Result<()> {
        let mut all_handles = Vec::new();
        // Per file: (relative_path, one Download per chunk, that chunk's
        // temp path, whether it needs zlib decompression, its source
        // URL for error messages). Chunks download independently, then
        // get concatenated in order once every chunk for that file has
        // landed — see `reassemble_file`.
        let mut file_downloads = Vec::with_capacity(files.len());

        for file in &files {
            let mut chunk_downloads = Vec::with_capacity(file.chunks.len());
            let mut chunk_temp_paths = Vec::with_capacity(file.chunks.len());
            let mut chunk_compressed = Vec::with_capacity(file.chunks.len());
            let mut chunk_urls = Vec::with_capacity(file.chunks.len());

            for (index, chunk) in file.chunks.iter().enumerate() {
                let url = url::Url::parse(&chunk.download_url).map_err(|err| {
                    LibraryError::InvalidChunkUrl {
                        file: file.relative_path.clone(),
                        source: err,
                    }
                })?;
                let temp_path = staging_dir.join(".chunks").join(format!(
                    "{}.{index}",
                    file.relative_path.replace(['/', '\\'], "_")
                ));
                if let Some(parent) = temp_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }

                let download = self
                    .downloads
                    .download(url, temp_path.clone())
                    .map_err(|err| LibraryError::EnqueueFailed {
                        file: file.relative_path.clone(),
                        source: err,
                    })?;
                all_handles.push(download.clone());

                let relative_path = file.relative_path.clone();
                let progress_tx = tx.clone();
                let progress_download = download.clone();
                tokio::spawn(async move {
                    let stream = progress_download.progress();
                    tokio::pin!(stream);
                    while let Some(progress) = stream.next().await {
                        let event = InstallEvent::Progress(InstallProgress {
                            current_file: relative_path.clone(),
                            bytes_downloaded: progress.bytes_downloaded(),
                            total_bytes: progress.total_bytes(),
                            bytes_per_second: progress.bytes_per_second(),
                        });
                        if progress_tx.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                });

                chunk_downloads.push(download);
                chunk_temp_paths.push(temp_path);
                chunk_compressed.push(chunk.compressed);
                chunk_urls.push(chunk.download_url.clone());
            }

            file_downloads.push((
                file.relative_path.clone(),
                chunk_downloads,
                chunk_temp_paths,
                chunk_compressed,
                chunk_urls,
            ));
        }

        self.active.lock().await.insert(key.clone(), all_handles);

        let mut relative_paths = Vec::with_capacity(file_downloads.len());
        let mut install_size_bytes = 0u64;

        let result: Result<()> = 'files: {
            for (relative_path, chunk_downloads, chunk_temp_paths, chunk_compressed, chunk_urls) in
                file_downloads
            {
                for (download, url) in chunk_downloads.iter().zip(&chunk_urls) {
                    if let Err(err) = download.clone().await {
                        break 'files Err(LibraryError::ChunkDownloadFailed {
                            file: relative_path.clone(),
                            url: url.clone(),
                            source: err,
                        }
                        .into());
                    }
                }

                let destination = destination_root.join(&relative_path);
                match reassemble_file(&destination, &chunk_temp_paths, &chunk_compressed).await {
                    Ok(size) => {
                        // Recorded with `install_root_name` baked in, so
                        // it's already a full path relative to the
                        // shared drive — callers never need to know
                        // `destination_root` separately.
                        relative_paths.push(format!("{install_root_name}/{relative_path}"));
                        install_size_bytes += size;
                    }
                    Err(err) => break 'files Err(crate::error::Error::from(err)),
                }
            }
            Ok(())
        };

        // The download is no longer cancellable once every file has
        // settled (succeeded, failed, or was already cancelled by
        // `cancel`, which removes this entry itself).
        self.active.lock().await.remove(&key);
        result?;

        let _ = tx
            .send(Ok(InstallEvent::Done {
                relative_paths,
                install_size_bytes,
            }))
            .await;
        Ok(())
    }

    /// Cancels a still-running download matching `key`, if any. A no-op
    /// if the download already finished (or was never started).
    pub async fn cancel(&self, key: &InstallKey) {
        if let Some(downloads) = self.active.lock().await.remove(key) {
            for download in downloads {
                let _ = download.cancel().await;
            }
        }
    }
}

/// Concatenates a file's already-downloaded chunks, in order, into
/// `destination`, decompressing each independently first when its
/// manifest entry said it needed it (matches how GOG's — and Epic's —
/// chunk formats work: each chunk is compressed on its own, not the
/// concatenated whole). Returns the reassembled file's size. Leaves temp
/// chunk files in place on error so a retry doesn't have to re-download
/// them; removes them on success.
async fn reassemble_file(
    destination: &std::path::Path,
    chunk_temp_paths: &[PathBuf],
    chunk_compressed: &[bool],
) -> std::io::Result<u64> {
    use tokio::io::AsyncWriteExt;

    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut out = tokio::fs::File::create(destination).await?;
    let mut total = 0u64;

    for (temp_path, &compressed) in chunk_temp_paths.iter().zip(chunk_compressed) {
        let bytes = tokio::fs::read(temp_path).await?;
        let bytes = if compressed {
            tokio::task::spawn_blocking(move || {
                use std::io::Read;
                let mut decoder = flate2::read::ZlibDecoder::new(&bytes[..]);
                let mut decompressed = Vec::new();
                decoder.read_to_end(&mut decompressed)?;
                std::io::Result::Ok(decompressed)
            })
            .await
            .map_err(std::io::Error::other)??
        } else {
            bytes
        };

        total += bytes.len() as u64;
        out.write_all(&bytes).await?;
    }

    for temp_path in chunk_temp_paths {
        let _ = tokio::fs::remove_file(temp_path).await;
    }

    Ok(total)
}
