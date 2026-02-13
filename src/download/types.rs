//! Download manager types and core data structures
//!
//! This module defines the fundamental types used throughout the download manager,
//! including task identifiers, states, progress updates, and configuration.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use url::Url;
use uuid::Uuid;

/// Unique identifier for a download task
///
/// TaskIds are generated using UUID v4 and are unique across the lifetime
/// of the application. They can be serialized for persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub Uuid);

impl TaskId {
    /// Generate a new unique task ID
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The state of a download task
///
/// Tasks progress through various states during their lifecycle:
/// - `Pending`: Task is queued but not yet started
/// - `Running`: Task is actively downloading
/// - `Pausing`: Pause requested, will pause after current chunk (notifies user)
/// - `Paused`: Task is paused and can be resumed
/// - `Completed`: Task finished successfully
/// - `Failed`: Task failed with an error
/// - `Cancelled`: Task was cancelled by user
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    /// Task is waiting in the queue to start
    Pending,
    /// Task is actively downloading
    Running,
    /// Pause requested, will pause after current chunk completes
    Pausing,
    /// Task is paused and can be resumed from the .part file
    Paused,
    /// Task completed successfully
    Completed,
    /// Task failed with an error
    Failed,
    /// Task was cancelled by user
    Cancelled,
}

impl TaskState {
    /// Check if the task is in a terminal state (cannot be resumed)
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskState::Completed | TaskState::Failed | TaskState::Cancelled)
    }

    /// Check if the task can be paused
    pub fn can_pause(&self) -> bool {
        matches!(self, TaskState::Running)
    }

    /// Check if the task can be resumed
    pub fn can_resume(&self) -> bool {
        matches!(self, TaskState::Paused | TaskState::Pausing)
    }

    /// Check if the task can be cancelled
    pub fn can_cancel(&self) -> bool {
        !self.is_terminal()
    }

    /// Get display name for the state
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskState::Pending => "Pending",
            TaskState::Running => "Running",
            TaskState::Pausing => "Pausing",
            TaskState::Paused => "Paused",
            TaskState::Completed => "Completed",
            TaskState::Failed => "Failed",
            TaskState::Cancelled => "Cancelled",
        }
    }
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Progress information for a download task
///
/// Provides real-time information about download progress including
/// bytes downloaded, total size, transfer speed, and estimated time remaining.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProgressUpdate {
    /// Number of bytes downloaded so far
    pub bytes_downloaded: u64,
    /// Total size of the file in bytes (None if unknown)
    pub total_bytes: Option<u64>,
    /// Current download speed in bytes per second
    pub bytes_per_second: f64,
    /// Estimated time remaining in seconds (None if unknown)
    pub eta_seconds: Option<u64>,
    /// Progress percentage (0.0 to 100.0, or None if total unknown)
    pub percentage: Option<f64>,
    /// Timestamp when this update was generated
    pub timestamp: SystemTime,
}

impl ProgressUpdate {
    /// Create a new progress update with default values
    pub fn new(bytes_downloaded: u64, total_bytes: Option<u64>) -> Self {
        let percentage = total_bytes.map(|total| {
            if total == 0 {
                100.0
            } else {
                (bytes_downloaded as f64 / total as f64) * 100.0
            }
        });

        Self {
            bytes_downloaded,
            total_bytes,
            bytes_per_second: 0.0,
            eta_seconds: None,
            percentage,
            timestamp: SystemTime::now(),
        }
    }

    /// Update speed and ETA based on previous progress
    pub fn update_speed(&mut self, previous: &ProgressUpdate, elapsed: Duration) {
        if elapsed.as_secs_f64() > 0.0 {
            let bytes_diff = self.bytes_downloaded.saturating_sub(previous.bytes_downloaded);
            self.bytes_per_second = bytes_diff as f64 / elapsed.as_secs_f64();

            if let Some(total) = self.total_bytes {
                let remaining = total.saturating_sub(self.bytes_downloaded);
                if self.bytes_per_second > 0.0 {
                    self.eta_seconds = Some((remaining as f64 / self.bytes_per_second) as u64);
                }
            }
        }
    }
}

impl Default for ProgressUpdate {
    fn default() -> Self {
        Self::new(0, None)
    }
}

/// Configuration for the download manager
///
/// Controls worker count, retry behavior, and other global settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadConfig {
    /// Maximum number of concurrent downloads
    pub max_concurrent_downloads: usize,
    /// Default directory for downloads
    pub default_download_dir: PathBuf,
    /// Number of retry attempts for failed downloads
    pub max_retries: u32,
    /// Initial retry delay (exponential backoff starts here)
    pub retry_delay_ms: u64,
    /// Maximum retry delay
    pub max_retry_delay_ms: u64,
    /// Chunk size for downloads in bytes
    pub chunk_size: usize,
    /// Whether to start workers automatically when tasks are added
    pub auto_start: bool,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            max_concurrent_downloads: 4,
            default_download_dir: std::env::temp_dir(),
            max_retries: 3,
            retry_delay_ms: 1000,
            max_retry_delay_ms: 60000,
            chunk_size: 8192, // 8KB chunks
            auto_start: true,
        }
    }
}

/// Represents a single download task
///
/// Contains all metadata and state for a download including the URL,
/// destination path, current state, progress, and retry information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTask {
    /// Unique identifier for this task
    pub id: TaskId,
    /// URL to download from
    pub url: Url,
    /// Final destination path for the downloaded file
    pub destination: PathBuf,
    /// Temporary file path for partial downloads (.part file)
    pub temp_path: PathBuf,
    /// Current state of the task
    pub state: TaskState,
    /// Current progress information
    pub progress: ProgressUpdate,
    /// Number of retry attempts made
    pub retry_count: u32,
    /// Maximum number of retry attempts allowed
    pub max_retries: u32,
    /// Time when the task was created
    pub created_at: SystemTime,
    /// Time when the task started downloading
    pub started_at: Option<SystemTime>,
    /// Time when the task completed, failed, or was cancelled
    pub completed_at: Option<SystemTime>,
    /// Error message if the task failed
    pub error: Option<String>,
    /// Whether to resume from partial download if available
    pub resume_support: bool,
    /// HTTP headers to include in the request
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<Vec<(String, String)>>,
}

impl DownloadTask {
    /// Create a new download task
    ///
    /// # Arguments
    ///
    /// * `url` - The URL to download from
    /// * `destination` - The final path where the file should be saved
    /// * `config` - Download configuration
    ///
    /// # Returns
    ///
    /// A new DownloadTask in the Pending state
    pub fn new(url: Url, destination: PathBuf, config: &DownloadConfig) -> Self {
        let id = TaskId::new();
        // Append .part to the filename to create temp path
        let temp_path = if let Some(stem) = destination.file_stem() {
            let mut new_name = stem.to_os_string();
            if let Some(ext) = destination.extension() {
                new_name.push(".");
                new_name.push(ext);
            }
            new_name.push(".part");
            destination.with_file_name(new_name)
        } else {
            destination.with_extension("part")
        };

        Self {
            id,
            url,
            destination,
            temp_path,
            state: TaskState::Pending,
            progress: ProgressUpdate::default(),
            retry_count: 0,
            max_retries: config.max_retries,
            created_at: SystemTime::now(),
            started_at: None,
            completed_at: None,
            error: None,
            resume_support: true,
            headers: None,
        }
    }

    /// Get the filename from the destination path
    pub fn filename(&self) -> Option<&str> {
        self.destination.file_name()?.to_str()
    }

    /// Check if this task can be resumed from a partial download
    pub fn can_resume_from_partial(&self) -> bool {
        self.resume_support && self.temp_path.exists()
    }

    /// Get the number of bytes already downloaded (for resume)
    pub fn bytes_already_downloaded(&self) -> u64 {
        if let Ok(metadata) = std::fs::metadata(&self.temp_path) {
            metadata.len()
        } else {
            0
        }
    }
}

/// Event emitted when a task's state changes
///
/// This is used for event hooks and subscriptions to track
/// state transitions in real-time.
#[derive(Debug, Clone)]
pub struct StateChangeEvent {
    /// The task ID that changed state
    pub task_id: TaskId,
    /// The previous state
    pub old_state: TaskState,
    /// The new state
    pub new_state: TaskState,
    /// Timestamp of the change
    pub timestamp: SystemTime,
}

impl StateChangeEvent {
    pub fn new(task_id: TaskId, old_state: TaskState, new_state: TaskState) -> Self {
        Self {
            task_id,
            old_state,
            new_state,
            timestamp: SystemTime::now(),
        }
    }
}

/// Command sent to workers to control task execution
#[derive(Debug, Clone)]
pub(crate) enum TaskCommand {
    /// Pause the task after current chunk
    Pause,
    /// Resume a paused task
    Resume,
    /// Cancel the task immediately
    Cancel,
}

/// Internal message passed through worker channels
#[derive(Debug)]
pub(crate) struct WorkerMessage {
    pub task: Arc<std::sync::Mutex<DownloadTask>>,
    pub command: Option<TaskCommand>,
}

/// Serializable representation of the entire queue state
///
/// Used for persistence and crash recovery. Can be exported to JSON
/// and imported later to restore the queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueState {
    /// Version of the state format for migration compatibility
    pub version: u32,
    /// List of all tasks in the queue
    pub tasks: Vec<DownloadTask>,
    /// Configuration used when state was saved
    pub config: DownloadConfig,
    /// Timestamp when state was saved
    pub saved_at: SystemTime,
}

impl QueueState {
    pub fn new(tasks: Vec<DownloadTask>, config: DownloadConfig) -> Self {
        Self {
            version: 1,
            tasks,
            config,
            saved_at: SystemTime::now(),
        }
    }
}

/// Statistics for all downloads
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct DownloadStats {
    /// Total number of tasks
    pub total_tasks: usize,
    /// Number of pending tasks
    pub pending: usize,
    /// Number of running tasks
    pub running: usize,
    /// Number of paused tasks
    pub paused: usize,
    /// Number of completed tasks
    pub completed: usize,
    /// Number of failed tasks
    pub failed: usize,
    /// Number of cancelled tasks
    pub cancelled: usize,
    /// Total bytes downloaded across all completed tasks
    pub total_bytes_downloaded: u64,
    /// Total download speed (bytes per second)
    pub total_speed: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_id_generation() {
        let id1 = TaskId::new();
        let id2 = TaskId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_task_state_transitions() {
        assert!(TaskState::Running.can_pause());
        assert!(!TaskState::Pending.can_pause());
        assert!(TaskState::Paused.can_resume());
        assert!(!TaskState::Running.can_resume());
        assert!(TaskState::Running.can_cancel());
        assert!(!TaskState::Completed.can_cancel());
    }

    #[test]
    fn test_progress_update() {
        let progress = ProgressUpdate::new(500, Some(1000));
        assert_eq!(progress.bytes_downloaded, 500);
        assert_eq!(progress.total_bytes, Some(1000));
        assert_eq!(progress.percentage, Some(50.0));
    }

    #[test]
    fn test_progress_update_zero_total() {
        let progress = ProgressUpdate::new(0, Some(0));
        assert_eq!(progress.percentage, Some(100.0));
    }

    #[test]
    fn test_download_task_creation() {
        let config = DownloadConfig::default();
        let url = Url::parse("https://dummyimage.com/6000x4000/000/fff.png&text=test").unwrap();
        let dest = PathBuf::from("/tmp/file.txt");
        
        let task = DownloadTask::new(url.clone(), dest.clone(), &config);
        
        assert_eq!(task.url, url);
        assert_eq!(task.destination, dest);
        assert_eq!(task.temp_path, PathBuf::from("/tmp/file.txt.part"));
        assert_eq!(task.state, TaskState::Pending);
        assert_eq!(task.max_retries, config.max_retries);
    }
}
