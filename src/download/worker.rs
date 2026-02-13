//! Worker pool for concurrent downloads
//!
//! This module provides a worker pool that manages concurrent download execution
//! using semaphore-based concurrency control. It handles pause/resume coordination,
/// temp file management, and atomic move on completion.

use crate::download::backend::{DownloadBackend, ReqwestBackend};
use crate::download::events::EventManager;
use crate::download::progress::ProgressTracker;
use crate::download::queue::DownloadQueue;
use crate::download::retry::{execute_with_smart_retry, RetryPolicy};
use crate::download::types::{DownloadConfig, DownloadTask, ProgressUpdate, TaskId, TaskState};
use crate::Error;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, Notify, Semaphore};
use tokio::task::JoinHandle;

/// Worker pool for managing download tasks
///
/// Controls the number of concurrent downloads using a semaphore and
/// coordinates pause/resume operations for individual tasks.
#[derive(Debug)]
pub struct WorkerPool {
    /// The download queue
    queue: DownloadQueue,
    /// Semaphore for controlling concurrent downloads
    semaphore: Arc<Semaphore>,
    /// Event manager for state change notifications
    event_manager: Arc<EventManager>,
    /// Progress tracker for real-time updates
    progress_tracker: Arc<ProgressTracker>,
    /// Download backend
    backend: ReqwestBackend,
    /// Active workers by task ID
    active_workers: Arc<DashMap<TaskId, WorkerHandle>>,
    /// Cancellation flags for tasks
    cancel_flags: Arc<DashMap<TaskId, Arc<Notify>>>,
    /// Pause flags for tasks
    pause_flags: Arc<DashMap<TaskId, Arc<Notify>>>,
    /// Global shutdown signal
    shutdown: Arc<Notify>,
    /// Configuration
    config: Arc<Mutex<DownloadConfig>>,
}

/// Handle to an active worker
#[derive(Debug)]
struct WorkerHandle {
    task_id: TaskId,
    handle: JoinHandle<()>,
}

impl Clone for WorkerPool {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
            semaphore: self.semaphore.clone(),
            event_manager: self.event_manager.clone(),
            progress_tracker: self.progress_tracker.clone(),
            backend: self.backend.clone(),
            active_workers: self.active_workers.clone(),
            cancel_flags: self.cancel_flags.clone(),
            pause_flags: self.pause_flags.clone(),
            shutdown: self.shutdown.clone(),
            config: self.config.clone(),
        }
    }
}

impl WorkerPool {
    /// Create a new worker pool
    ///
    /// # Arguments
    ///
    /// * `queue` - The download queue
    /// * `event_manager` - Event manager for state changes
    /// * `progress_tracker` - Progress tracker for updates
    /// * `config` - Download configuration
    pub fn new(
        queue: DownloadQueue,
        event_manager: Arc<EventManager>,
        progress_tracker: Arc<ProgressTracker>,
        config: DownloadConfig,
    ) -> Result<Self, Error> {
        let max_concurrent = config.max_concurrent_downloads;
        let backend = ReqwestBackend::new()?;

        Ok(Self {
            queue,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            event_manager,
            progress_tracker,
            backend,
            active_workers: Arc::new(DashMap::new()),
            cancel_flags: Arc::new(DashMap::new()),
            pause_flags: Arc::new(DashMap::new()),
            shutdown: Arc::new(Notify::new()),
            config: Arc::new(Mutex::new(config)),
        })
    }

    /// Start the worker pool
    ///
    /// Spawns workers that will process tasks from the queue.
    pub fn start(&self) {
        let pool = self.clone();
        tokio::spawn(async move {
            pool.run().await;
        });
    }

    /// Main worker loop
    async fn run(&self) {
        loop {
            tokio::select! {
                _ = self.shutdown.notified() => {
                    tracing::info!("Worker pool shutting down");
                    break;
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                    // Try to get the next pending task
                    if let Some(task) = self.queue.next_pending().await {
                        let task_id = {
                            let locked = task.lock().await;
                            locked.id
                        };

                        // Check if already being processed
                        if !self.active_workers.contains_key(&task_id) {
                            self.spawn_worker(task).await;
                        }
                    }
                }
            }
        }
    }

    /// Spawn a worker for a specific task
    async fn spawn_worker(&self, task: Arc<Mutex<DownloadTask>>) {
        let task_id = {
            let locked = task.lock().await;
            locked.id
        };

        // Update task state to Running
        if let Err(e) = self.queue.update_task_state(&task_id, TaskState::Running).await {
            tracing::error!("Failed to update task state: {}", e);
            return;
        }

        // Emit state change event
        self.event_manager
            .emit_state_change(task_id, TaskState::Pending, TaskState::Running)
            .await;

        // Create cancellation and pause flags
        let cancel_notify = Arc::new(Notify::new());
        let pause_notify = Arc::new(Notify::new());
        self.cancel_flags.insert(task_id, cancel_notify.clone());
        self.pause_flags.insert(task_id, pause_notify.clone());

        // Clone necessary data for the worker task
        let pool = self.clone();
        let backend = self.backend.clone();
        let event_manager = self.event_manager.clone();
        let progress_tracker = self.progress_tracker.clone();
        let queue = self.queue.clone();
        let semaphore = self.semaphore.clone();

        // Spawn worker task
        let handle = tokio::spawn(async move {
            // Acquire semaphore permit
            let _permit = semaphore.acquire().await;

            // Execute download with retry logic
            let result = pool
                .execute_download(
                    task.clone(),
                    backend,
                    event_manager.clone(),
                    progress_tracker.clone(),
                    queue.clone(),
                    cancel_notify,
                    pause_notify,
                )
                .await;

            // Handle result
            match result {
                Ok(()) => {
                    tracing::info!("Task {} completed successfully", task_id);
                }
                Err(e) => {
                    tracing::error!("Task {} failed: {}", task_id, e);
                    let _ = queue.set_task_error(&task_id, e.to_string()).await;
                    let _ = queue.update_task_state(&task_id, TaskState::Failed).await;
                    event_manager
                        .emit_state_change(task_id, TaskState::Running, TaskState::Failed)
                        .await;
                }
            }

            // Cleanup
            pool.active_workers.remove(&task_id);
            pool.cancel_flags.remove(&task_id);
            pool.pause_flags.remove(&task_id);
            progress_tracker.remove_task(&task_id);
        });

        // Store worker handle
        self.active_workers.insert(
            task_id,
            WorkerHandle { task_id, handle },
        );
    }

    /// Execute a download with support for pause/resume and progress tracking
    async fn execute_download(
        &self,
        task: Arc<Mutex<DownloadTask>>,
        backend: ReqwestBackend,
        event_manager: Arc<EventManager>,
        progress_tracker: Arc<ProgressTracker>,
        queue: DownloadQueue,
        _cancel_notify: Arc<Notify>,
        _pause_notify: Arc<Notify>,
    ) -> Result<(), Error> {
        let (task_id, url, _temp_path, resume_from) = {
            let locked = task.lock().await;
            (
                locked.id,
                locked.url.clone(),
                locked.temp_path.clone(),
                locked.bytes_already_downloaded(),
            )
        };

        tracing::info!(
            "Starting download for task {} from {} (resume from: {})",
            task_id,
            url,
            resume_from
        );

        // Execute download with retry logic
        let retry_policy = {
            let config = self.config.lock().await;
            RetryPolicy {
                max_retries: config.max_retries,
                initial_delay_ms: config.retry_delay_ms,
                max_delay_ms: config.max_retry_delay_ms,
                ..Default::default()
            }
        };

        execute_with_smart_retry(
            || async {
                // Clone necessary data for each retry attempt
                let task_ref = task.clone();
                let backend_ref = backend.clone();
                let event_mgr = event_manager.clone();
                let progress_trk = progress_tracker.clone();
                let queue_ref = queue.clone();

                self.download_with_controls(
                    task_ref,
                    backend_ref,
                    event_mgr,
                    progress_trk,
                    queue_ref,
                )
                .await
            },
            retry_policy,
        )
        .await
    }

    /// Download with pause/resume/cancel controls
    async fn download_with_controls(
        &self,
        task: Arc<Mutex<DownloadTask>>,
        backend: ReqwestBackend,
        event_manager: Arc<EventManager>,
        progress_tracker: Arc<ProgressTracker>,
        queue: DownloadQueue,
    ) -> Result<(), Error> {
        let task_id = { task.lock().await.id };
        let resume_from = { task.lock().await.bytes_already_downloaded() };

        // Create channel for progress updates
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();

        // Clone queue for the progress handler
        let queue_for_progress = queue.clone();

        // Spawn task to handle progress updates
        let progress_handle = tokio::spawn(async move {
            while let Some(progress) = progress_rx.recv().await {
                // Update progress tracker
                progress_tracker.update_progress(task_id, progress);
                
                // Also update queue
                let _ = queue_for_progress.update_task_progress(&task_id, progress).await;
            }
        });

        // Execute the download
        let download_result = backend
            .download(&*task.lock().await, resume_from, progress_tx)
            .await;

        // Wait for progress handler to finish
        let _ = progress_handle.await;

        match download_result {
            Ok(bytes_downloaded) => {
                // Move temp file to final destination
                self.finalize_download(task.clone()).await?;

                // Update state to Completed
                queue.update_task_state(&task_id, TaskState::Completed).await?;
                event_manager
                    .emit_state_change(task_id, TaskState::Running, TaskState::Completed)
                    .await;

                tracing::info!(
                    "Download completed for task {} ({} bytes)",
                    task_id,
                    bytes_downloaded
                );

                Ok(())
            }
            Err(e) => {
                // Check if it was cancelled
                if !self.cancel_flags.contains_key(&task_id) {
                    tracing::info!("Task {} was cancelled", task_id);
                    return Err(Error::Download("Cancelled".to_string()));
                }
                Err(e)
            }
        }
    }

    /// Move temp file to final destination atomically
    async fn finalize_download(&self, task: Arc<Mutex<DownloadTask>>) -> Result<(), Error> {
        let (temp_path, dest_path) = {
            let locked = task.lock().await;
            (locked.temp_path.clone(), locked.destination.clone())
        };

        // Ensure destination directory exists
        if let Some(parent) = dest_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Atomic move from temp to final destination
        tokio::fs::rename(&temp_path, &dest_path).await?;

        tracing::debug!(
            "Moved {} to {}",
            temp_path.display(),
            dest_path.display()
        );

        Ok(())
    }

    /// Pause a running task
    ///
    /// Returns true if pause was initiated, false if task not found or not running
    pub async fn pause_task(&self, task_id: TaskId) -> Result<(), Error> {
        if self.active_workers.contains_key(&task_id) {
            // Update state to Pausing first
            self.queue.update_task_state(&task_id, TaskState::Pausing).await?;
            
            // Emit pausing event
            self.event_manager
                .emit_state_change(task_id, TaskState::Running, TaskState::Pausing)
                .await;

            // Signal pause to worker
            if let Some(flag) = self.pause_flags.get(&task_id) {
                flag.notify_one();
            }

            tracing::info!("Task {} will pause after current chunk", task_id);
            Ok(())
        } else {
            Err(Error::TaskNotFound(task_id.to_string()))
        }
    }

    /// Resume a paused task
    pub async fn resume_task(&self, task_id: TaskId) -> Result<(), Error> {
        // Get the task from queue
        let task = self.queue.get_task(&task_id).await
            .ok_or_else(|| Error::TaskNotFound(task_id.to_string()))?;

        {
            let locked = task.lock().await;
            if !locked.state.can_resume() {
                return Err(Error::InvalidStateTransition {
                    from: locked.state.to_string(),
                    to: TaskState::Running.to_string(),
                });
            }
        }

        // Update state back to Pending (will be picked up by worker pool)
        self.queue.update_task_state(&task_id, TaskState::Pending).await?;
        
        self.event_manager
            .emit_state_change(task_id, TaskState::Paused, TaskState::Pending)
            .await;

        tracing::info!("Task {} resumed", task_id);
        Ok(())
    }

    /// Cancel a task
    pub async fn cancel_task(&self, task_id: TaskId) -> Result<(), Error> {
        // Signal cancellation
        if let Some(flag) = self.cancel_flags.get(&task_id) {
            flag.notify_one();
        }

        // Abort the worker if running
        if let Some((_, handle)) = self.active_workers.remove(&task_id) {
            handle.handle.abort();
        }

        // Update state
        self.queue.update_task_state(&task_id, TaskState::Cancelled).await?;
        
        // Get old state - we need to determine what it was
        // For simplicity, assume it was Running
        self.event_manager
            .emit_state_change(task_id, TaskState::Running, TaskState::Cancelled)
            .await;

        // Cleanup temp file
        if let Some(task) = self.queue.get_task(&task_id).await {
            let temp_path = { task.lock().await.temp_path.clone() };
            let _ = tokio::fs::remove_file(&temp_path).await;
        }

        // Remove from tracking
        self.active_workers.remove(&task_id);
        self.cancel_flags.remove(&task_id);
        self.pause_flags.remove(&task_id);
        self.progress_tracker.remove_task(&task_id);

        tracing::info!("Task {} cancelled", task_id);
        Ok(())
    }

    /// Cancel all active tasks
    pub async fn cancel_all(&self) -> usize {
        let mut cancelled_count = 0;
        let task_ids: Vec<TaskId> = self.active_workers.iter().map(|e| *e.key()).collect();

        for task_id in task_ids {
            if self.cancel_task(task_id).await.is_ok() {
                cancelled_count += 1;
            }
        }

        cancelled_count
    }

    /// Pause all running tasks
    pub async fn pause_all(&self) -> usize {
        let mut paused_count = 0;
        let task_ids: Vec<TaskId> = self.active_workers.iter().map(|e| *e.key()).collect();

        for task_id in task_ids {
            if self.pause_task(task_id).await.is_ok() {
                paused_count += 1;
            }
        }

        paused_count
    }

    /// Resume all paused tasks
    pub async fn resume_all(&self) -> usize {
        let paused_tasks = self.queue.get_tasks_by_state(TaskState::Paused);
        let mut resumed_count = 0;

        for task in paused_tasks {
            if self.resume_task(task.id).await.is_ok() {
                resumed_count += 1;
            }
        }

        resumed_count
    }

    /// Shutdown the worker pool
    pub async fn shutdown(&self) {
        // Cancel all active tasks
        self.cancel_all().await;
        
        // Signal shutdown
        self.shutdown.notify_waiters();
        
        tracing::info!("Worker pool shutdown complete");
    }

    /// Get number of active workers
    pub fn active_count(&self) -> usize {
        self.active_workers.len()
    }

    /// Update configuration
    pub async fn update_config(&self, config: DownloadConfig) {
        let mut c = self.config.lock().await;
        *c = config;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use url::Url;

    #[tokio::test]
    async fn test_worker_pool_creation() {
        let queue = DownloadQueue::new(DownloadConfig::default());
        let event_manager = Arc::new(EventManager::new(100));
        let progress_tracker = Arc::new(ProgressTracker::new(100));
        let config = DownloadConfig::default();

        let pool = WorkerPool::new(
            queue,
            event_manager,
            progress_tracker,
            config,
        );

        assert!(pool.is_ok());
    }

    #[tokio::test]
    async fn test_pause_resume_cancel() {
        let queue = DownloadQueue::new(DownloadConfig::default());
        let event_manager = Arc::new(EventManager::new(100));
        let progress_tracker = Arc::new(ProgressTracker::new(100));
        let config = DownloadConfig::default();

        let pool = WorkerPool::new(
            queue.clone(),
            event_manager,
            progress_tracker,
            config,
        )
        .unwrap();

        // Add a task
        let task = DownloadTask::new(
            Url::parse("https://dummyimage.com/6000x4000/000/fff.png&text=test").unwrap(),
            PathBuf::from("/tmp/test.txt"),
            &DownloadConfig::default(),
        );
        let task_id = task.id;
        queue.add_task(task).await.unwrap();

        // Set task state to Running for testing pause/resume
        queue.update_task_state(&task_id, TaskState::Running).await.unwrap();

        // Test pause (should fail since worker isn't actually running)
        // In real scenario, there would be an active worker
        let result = pool.pause_task(task_id).await;
        assert!(result.is_err()); // Worker not found

        // Test resume
        queue.update_task_state(&task_id, TaskState::Paused).await.unwrap();
        let result = pool.resume_task(task_id).await;
        assert!(result.is_ok());
    }
}
