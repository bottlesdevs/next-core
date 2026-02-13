//! Download Manager - Main API
//!
//! This module provides the main `DownloadManager` struct which is the primary
//! interface for managing downloads. It manages the queue, workers, configuration,
//! and global state with a thread-safe, clonable design.
//!
//! # Example
//!
//! ```rust,no_run
//! use bottles_core::download::{DownloadManager, DownloadConfig};
//! use std::path::PathBuf;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create manager with default config
//!     let manager = DownloadManager::new(DownloadConfig::default())?;
//!
//!     // Add a download
//!     let task_id = manager
//!         .add_url("https://example.com/file.zip")
//!         .await?;
//!
//!     // Subscribe to progress
//!     let mut progress_rx = manager.subscribe_progress(task_id);
//!     tokio::spawn(async move {
//!         while progress_rx.changed().await.is_ok() {
//!             let progress = progress_rx.borrow().clone();
//!             println!("Progress: {:.1}%", progress.percentage.unwrap_or(0.0));
//!         }
//!     });
//!
//!     // Wait for completion (in real code, you'd wait for the task to complete)
//!     tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
//!
//!     Ok(())
//! }
//! ```

use crate::download::events::EventManager;
use crate::download::types::StateChangeEvent;
use crate::download::progress::ProgressTracker;
use crate::download::queue::DownloadQueue;
use crate::download::types::{
    DownloadConfig, DownloadStats, DownloadTask, ProgressUpdate, TaskId, TaskState,
};
use crate::download::worker::WorkerPool;
use crate::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, watch};
use url::Url;

/// Inner state of the download manager
///
/// This struct contains all the internal state that needs to be shared
/// across clones of the DownloadManager.
#[derive(Debug)]
struct InnerManager {
    /// Download queue
    queue: DownloadQueue,
    /// Worker pool for executing downloads
    worker_pool: Arc<Mutex<Option<WorkerPool>>>,
    /// Event manager for state changes
    event_manager: Arc<EventManager>,
    /// Progress tracker
    progress_tracker: Arc<ProgressTracker>,
    /// Configuration
    config: RwLock<DownloadConfig>,
    /// Whether the manager is running
    running: RwLock<bool>,
}

/// Download Manager
///
/// The main interface for managing file downloads. This struct is cheap to clone
/// and can be shared across multiple tasks. It manages a queue of downloads,
/// worker pool for concurrent execution, and provides real-time progress tracking.
///
/// # Cloning
///
/// The manager uses `Arc` internally, so cloning creates a new reference to the
/// same underlying state. All clones see the same queue, progress, and can
/// control the same downloads.
///
/// # Thread Safety
///
/// All operations are thread-safe and can be called concurrently from multiple
/// async tasks.
#[derive(Debug, Clone)]
pub struct DownloadManager {
    inner: Arc<InnerManager>,
}

impl DownloadManager {
    /// Create a new download manager with the given configuration
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for the download manager
    ///
    /// # Returns
    ///
    /// A new DownloadManager instance
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP backend cannot be initialized
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use bottles_core::download::{DownloadManager, DownloadConfig};
    ///
    /// let config = DownloadConfig::default();
    /// let manager = DownloadManager::new(config)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(config: DownloadConfig) -> Result<Self, Error> {
        let queue = DownloadQueue::new(config.clone());
        let event_manager = Arc::new(EventManager::new(1024));
        let progress_tracker = Arc::new(ProgressTracker::new(1024));

        let inner = InnerManager {
            queue,
            worker_pool: Arc::new(Mutex::new(None)),
            event_manager,
            progress_tracker,
            config: RwLock::new(config),
            running: RwLock::new(false),
        };

        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Start the download manager
    ///
    /// This initializes the worker pool and starts processing downloads.
    /// If `auto_start` is enabled in the config, this is called automatically
    /// when the first download is added.
    ///
    /// # Returns
    ///
    /// Returns Ok if already running or successfully started
    pub async fn start(&self) -> Result<(), Error> {
        let mut running = self.inner.running.write().await;
        if *running {
            return Ok(());
        }

        let config = self.inner.config.read().await.clone();
        
        // Create worker pool if not exists
        let mut pool_guard = self.inner.worker_pool.lock().await;
        if pool_guard.is_none() {
            let pool = WorkerPool::new(
                self.inner.queue.clone(),
                self.inner.event_manager.clone(),
                self.inner.progress_tracker.clone(),
                config,
            )?;
            *pool_guard = Some(pool);
        }

        // Start the worker pool
        if let Some(pool) = pool_guard.as_ref() {
            pool.start();
        }

        *running = true;
        tracing::info!("Download manager started");
        
        Ok(())
    }

    /// Stop the download manager
    ///
    /// Cancels all active downloads and shuts down the worker pool.
    /// The queue is preserved and can be resumed later with `start()`.
    pub async fn stop(&self) {
        let mut running = self.inner.running.write().await;
        
        if let Some(pool) = self.inner.worker_pool.lock().await.take() {
            pool.shutdown().await;
        }
        
        *running = false;
        tracing::info!("Download manager stopped");
    }

    /// Check if the manager is running
    pub async fn is_running(&self) -> bool {
        *self.inner.running.read().await
    }

    /// Add a URL to the download queue
    ///
    /// The file will be downloaded to the default download directory.
    /// The filename is extracted from the URL or Content-Disposition header.
    ///
    /// # Arguments
    ///
    /// * `url` - The URL to download
    ///
    /// # Returns
    ///
    /// The TaskId of the newly created download task
    pub async fn add_url(&self, url: impl AsRef<str>) -> Result<TaskId, Error> {
        let url = Url::parse(url.as_ref())?;
        let config = self.inner.config.read().await;
        
        // Extract filename from URL
        let filename = url
            .path_segments()
            .and_then(|segments| segments.last())
            .unwrap_or("download");
        
        let destination = config.default_download_dir.join(filename);
        
        self.add_url_with_destination(url, destination).await
    }

    /// Add a URL with a specific destination path
    ///
    /// # Arguments
    ///
    /// * `url` - The URL to download
    /// * `destination` - The full path where the file should be saved
    pub async fn add_url_with_destination(
        &self,
        url: impl AsRef<str>,
        destination: impl AsRef<Path>,
    ) -> Result<TaskId, Error> {
        let url = Url::parse(url.as_ref())?;
        let destination = destination.as_ref().to_path_buf();
        let config = self.inner.config.read().await.clone();

        let task = DownloadTask::new(url, destination, &config);
        let task_id = task.id;

        self.inner.queue.add_task(task).await?;

        // Auto-start if enabled
        if config.auto_start && !self.is_running().await {
            self.start().await?;
        }

        tracing::info!("Added download task {}", task_id);
        Ok(task_id)
    }

    /// Add multiple URLs to the download queue
    ///
    /// # Arguments
    ///
    /// * `urls` - Iterator of URLs to download
    ///
    /// # Returns
    ///
    /// Vector of TaskIds for the created tasks
    pub async fn add_urls(
        &self,
        urls: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Vec<Result<TaskId, Error>> {
        let mut results = Vec::new();
        for url in urls {
            results.push(self.add_url(url).await);
        }
        results
    }

    /// Pause a specific download task
    ///
    /// The task will pause after completing the current chunk download.
    /// A `StateChangeEvent` will be emitted with `Pausing` state to notify
    /// that the task will pause soon.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task to pause
    pub async fn pause_task(&self, task_id: TaskId) -> Result<(), Error> {
        let pool_guard = self.inner.worker_pool.lock().await;
        if let Some(pool) = pool_guard.as_ref() {
            pool.pause_task(task_id).await
        } else {
            Err(Error::Worker("Worker pool not running".to_string()))
        }
    }

    /// Resume a paused download task
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task to resume
    pub async fn resume_task(&self, task_id: TaskId) -> Result<(), Error> {
        let pool_guard = self.inner.worker_pool.lock().await;
        if let Some(pool) = pool_guard.as_ref() {
            pool.resume_task(task_id).await
        } else {
            Err(Error::Worker("Worker pool not running".to_string()))
        }
    }

    /// Cancel a specific download task
    ///
    /// The task will be cancelled immediately and any partial download
    /// will be deleted.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task to cancel
    pub async fn cancel_task(&self, task_id: TaskId) -> Result<(), Error> {
        let pool_guard = self.inner.worker_pool.lock().await;
        if let Some(pool) = pool_guard.as_ref() {
            pool.cancel_task(task_id).await
        } else {
            // If pool not running, just remove from queue
            self.inner.queue.update_task_state(&task_id, TaskState::Cancelled).await?;
            self.inner.queue.remove_task(&task_id).await?;
            Ok(())
        }
    }

    /// Pause all running download tasks
    ///
    /// # Returns
    ///
    /// The number of tasks that were paused
    pub async fn pause_all(&self) -> usize {
        let pool_guard = self.inner.worker_pool.lock().await;
        if let Some(pool) = pool_guard.as_ref() {
            pool.pause_all().await
        } else {
            0
        }
    }

    /// Resume all paused download tasks
    ///
    /// # Returns
    ///
    /// The number of tasks that were resumed
    pub async fn resume_all(&self) -> usize {
        let pool_guard = self.inner.worker_pool.lock().await;
        if let Some(pool) = pool_guard.as_ref() {
            pool.resume_all().await
        } else {
            // Just update queue states
            let paused_tasks = self.inner.queue.get_tasks_by_state(TaskState::Paused);
            let mut count = 0;
            for task in paused_tasks {
                if self.inner.queue.update_task_state(&task.id, TaskState::Pending).await.is_ok() {
                    count += 1;
                }
            }
            count
        }
    }

    /// Cancel all download tasks
    ///
    /// # Returns
    ///
    /// The number of tasks that were cancelled
    pub async fn cancel_all(&self) -> usize {
        let pool_guard = self.inner.worker_pool.lock().await;
        if let Some(pool) = pool_guard.as_ref() {
            pool.cancel_all().await
        } else {
            // Cancel all pending tasks
            let all_tasks = self.inner.queue.get_all_tasks();
            let mut count = 0;
            for task in all_tasks {
                if !task.state.is_terminal() {
                    let _ = self.inner.queue.update_task_state(&task.id, TaskState::Cancelled).await;
                    let _ = self.inner.queue.remove_task(&task.id).await;
                    count += 1;
                }
            }
            count
        }
    }

    /// Subscribe to progress updates for a specific task
    ///
    /// Returns a watch receiver that receives real-time progress updates.
    /// The receiver will receive updates as they happen without debouncing.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task to subscribe to
    ///
    /// # Returns
    ///
    /// A watch receiver for progress updates
    pub fn subscribe_progress(&self, task_id: TaskId) -> watch::Receiver<ProgressUpdate> {
        self.inner.progress_tracker.subscribe_task(task_id)
    }

    /// Subscribe to all progress updates
    ///
    /// Returns a broadcast receiver that receives updates for all tasks.
    /// Each update includes the TaskId and ProgressUpdate.
    pub fn subscribe_all_progress(
        &self,
    ) -> tokio::sync::broadcast::Receiver<(TaskId, ProgressUpdate)> {
        self.inner.progress_tracker.subscribe_all()
    }

    /// Subscribe to state change events
    ///
    /// Returns a broadcast receiver that receives all state change events.
    pub fn subscribe_state_changes(&self) -> tokio::sync::broadcast::Receiver<StateChangeEvent> {
        self.inner.event_manager.subscribe()
    }

    /// Register a callback for state change events
    ///
    /// The callback is called synchronously whenever a task's state changes.
    ///
    /// # Arguments
    ///
    /// * `callback` - The callback function
    pub async fn on_state_change<F>(&self, callback: F)
    where
        F: Fn(StateChangeEvent) + Send + Sync + 'static,
    {
        self.inner.event_manager.on_state_change(callback).await;
    }

    /// Get a task by ID
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task to retrieve
    pub async fn get_task(&self, task_id: TaskId) -> Option<DownloadTask> {
        self.inner
            .queue
            .get_task(&task_id)
            .await
            .and_then(|t| t.try_lock().ok().map(|g| g.clone()))
    }

    /// Get all tasks
    pub fn get_all_tasks(&self) -> Vec<DownloadTask> {
        self.inner.queue.get_all_tasks()
    }

    /// Get tasks filtered by state
    pub fn get_tasks_by_state(&self, state: TaskState) -> Vec<DownloadTask> {
        self.inner.queue.get_tasks_by_state(state)
    }

    /// Get queue statistics
    pub async fn stats(&self) -> DownloadStats {
        self.inner.queue.stats().await
    }

    /// Export queue state to a file
    ///
    /// Saves the current queue state including all tasks and configuration
    /// to a JSON file. This can be used for crash recovery.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to save the state file
    pub async fn export_state(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        self.inner.queue.export_to_file(path).await
    }

    /// Import queue state from a file
    ///
    /// Restores the queue state from a previously exported file.
    /// Only non-terminal tasks are imported.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the state file
    ///
    /// # Returns
    ///
    /// The number of tasks imported
    pub async fn import_state(&self, path: impl AsRef<Path>) -> Result<usize, Error> {
        let task_ids = self.inner.queue.import_from_file(path).await?;
        Ok(task_ids.len())
    }

    /// Get the current configuration
    pub async fn config(&self) -> DownloadConfig {
        self.inner.config.read().await.clone()
    }

    /// Update the configuration
    ///
    /// Note: Some configuration changes may not take effect until
    /// the next download starts.
    pub async fn set_config(&self, config: DownloadConfig) {
        let mut c = self.inner.config.write().await;
        *c = config.clone();
        
        // Update worker pool config if running
        if let Some(pool) = self.inner.worker_pool.lock().await.as_ref() {
            pool.update_config(config.clone()).await;
        }
        
        // Update queue config
        self.inner.queue.set_config(config).await;
    }

    /// Get the default download directory
    pub async fn default_download_dir(&self) -> PathBuf {
        self.inner.config.read().await.default_download_dir.clone()
    }

    /// Set the default download directory
    pub async fn set_default_download_dir(&self, dir: impl AsRef<Path>) {
        let mut config = self.inner.config.write().await;
        config.default_download_dir = dir.as_ref().to_path_buf();
    }

    /// Get the number of active downloads
    pub fn active_downloads(&self) -> usize {
        if let Ok(pool_guard) = self.inner.worker_pool.try_lock() {
            pool_guard.as_ref().map(|p| p.active_count()).unwrap_or(0)
        } else {
            0
        }
    }

    /// Wait for all downloads to complete
    ///
    /// Returns when all tasks reach a terminal state (Completed, Failed, or Cancelled).
    /// Note: New tasks added while waiting will also be waited for.
    pub async fn wait_for_all(&self) {
        loop {
            let stats = self.stats().await;
            let active = stats.running + stats.pending + stats.paused;
            
            if active == 0 {
                break;
            }
            
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }

    /// Clear completed tasks from the queue
    ///
    /// # Returns
    ///
    /// The number of tasks removed
    pub async fn clear_completed(&self) -> usize {
        self.inner.queue.clear_completed().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_manager_creation() {
        let config = DownloadConfig::default();
        let manager = DownloadManager::new(config);
        assert!(manager.is_ok());
    }

    #[tokio::test]
    async fn test_add_url() {
        let config = DownloadConfig::default();
        let manager = DownloadManager::new(config).unwrap();

        let task_id = manager.add_url("https://dummyimage.com/6000x4000/000/fff.png&text=test").await;
        assert!(task_id.is_ok());

        let task = manager.get_task(task_id.unwrap()).await;
        assert!(task.is_some());
    }

    #[tokio::test]
    async fn test_add_url_with_destination() {
        let config = DownloadConfig::default();
        let manager = DownloadManager::new(config).unwrap();

        let task_id = manager
            .add_url_with_destination("https://dummyimage.com/6000x4000/000/fff.png&text=test", "/tmp/test.txt")
            .await;
        assert!(task_id.is_ok());

        let task = manager.get_task(task_id.unwrap()).await.unwrap();
        assert_eq!(task.destination, PathBuf::from("/tmp/test.txt"));
    }

    #[tokio::test]
    async fn test_start_stop() {
        let config = DownloadConfig::default();
        let manager = DownloadManager::new(config).unwrap();

        assert!(!manager.is_running().await);

        manager.start().await.unwrap();
        assert!(manager.is_running().await);

        manager.stop().await;
        assert!(!manager.is_running().await);
    }

    #[tokio::test]
    async fn test_subscribe_state_changes() {
        let config = DownloadConfig::default();
        let manager = DownloadManager::new(config).unwrap();

        let mut rx = manager.subscribe_state_changes();

        // Add a task to trigger state change
        let _ = manager.add_url("https://dummyimage.com/6000x4000/000/fff.png&text=test").await;

        // In real scenario, we'd see state changes here
        // For this test, just verify the subscription works
        assert_eq!(rx.len(), 0); // No events yet since manager isn't started
    }

    #[tokio::test]
    async fn test_stats() {
        let config = DownloadConfig::default();
        let manager = DownloadManager::new(config).unwrap();

        let _ = manager.add_url("https://dummyimage.com/6000x4000/000/fff.png&text=test").await;
        let _ = manager.add_url("https://dummyimage.com/6000x4000/000/fff.png&text=test2").await;

        let stats = manager.stats().await;
        assert_eq!(stats.total_tasks, 2);
        assert_eq!(stats.pending, 2);
    }
}
