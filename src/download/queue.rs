//! Thread-safe download queue with state persistence
//!
//! This module provides a concurrent queue for managing download tasks,
//! supporting atomic operations, state persistence for crash recovery,
//! and partial download resume tracking.

use crate::download::types::{DownloadConfig, DownloadTask, QueueState, TaskId, TaskState};
use crate::Error;
use dashmap::DashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// Thread-safe download queue
///
/// Manages a collection of download tasks with concurrent access support.
/// Tasks can be added, removed, and updated from multiple threads safely.
/// The queue maintains FIFO order for pending tasks.
#[derive(Debug)]
pub struct DownloadQueue {
    /// Map of all tasks by ID for O(1) lookup
    tasks: Arc<DashMap<TaskId, Arc<Mutex<DownloadTask>>>>,
    /// FIFO queue of pending task IDs
    pending_queue: Arc<Mutex<VecDeque<TaskId>>>,
    /// Current configuration
    config: Arc<RwLock<DownloadConfig>>,
    /// Flag indicating if the queue is closed
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl Clone for DownloadQueue {
    fn clone(&self) -> Self {
        Self {
            tasks: self.tasks.clone(),
            pending_queue: self.pending_queue.clone(),
            config: self.config.clone(),
            closed: self.closed.clone(),
        }
    }
}

impl DownloadQueue {
    /// Create a new download queue with the given configuration
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for the queue
    ///
    /// # Example
    ///
    /// ```rust
    /// use bottles_core::download::{DownloadQueue, DownloadConfig};
    ///
    /// let config = DownloadConfig::default();
    /// let queue = DownloadQueue::new(config);
    /// ```
    pub fn new(config: DownloadConfig) -> Self {
        Self {
            tasks: Arc::new(DashMap::new()),
            pending_queue: Arc::new(Mutex::new(VecDeque::new())),
            config: Arc::new(RwLock::new(config)),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Add a task to the queue
    ///
    /// The task will be added to the pending queue and can be picked up
    /// by workers when they become available.
    ///
    /// # Arguments
    ///
    /// * `task` - The download task to add
    ///
    /// # Returns
    ///
    /// Returns the TaskId of the added task, or an error if the queue is closed
    /// or a task with the same ID already exists.
    pub async fn add_task(&self, task: DownloadTask) -> Result<TaskId, Error> {
        if self.is_closed() {
            return Err(Error::QueueClosed);
        }

        let task_id = task.id;

        // Check if task already exists
        if self.tasks.contains_key(&task_id) {
            return Err(Error::TaskExists(task_id.to_string()));
        }

        // Insert into tasks map
        self.tasks
            .insert(task_id, Arc::new(Mutex::new(task)));

        // Add to pending queue
        let mut queue = self.pending_queue.lock().await;
        queue.push_back(task_id);

        tracing::debug!("Added task {} to queue", task_id);
        Ok(task_id)
    }

    /// Get the next pending task from the queue
    ///
    /// Removes and returns the next task in FIFO order that is in the Pending state.
    ///
    /// # Returns
    ///
    /// Returns Some(task) if a pending task is available, None if the queue is empty
    /// or all tasks are in other states.
    pub async fn next_pending(&self) -> Option<Arc<Mutex<DownloadTask>>> {
        let mut queue = self.pending_queue.lock().await;

        while let Some(task_id) = queue.pop_front() {
            if let Some(task_entry) = self.tasks.get(&task_id) {
                let task = task_entry.value().lock().await;
                if task.state == TaskState::Pending {
                    drop(task);
                    return Some(task_entry.value().clone());
                }
            }
        }

        None
    }

    /// Get a task by ID
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task to retrieve
    ///
    /// # Returns
    ///
    /// Returns the task if found, None otherwise
    pub async fn get_task(&self, task_id: &TaskId) -> Option<Arc<Mutex<DownloadTask>>> {
        self.tasks.get(task_id).map(|entry| entry.value().clone())
    }

    /// Get all tasks
    ///
    /// Returns a snapshot of all tasks currently in the queue.
    pub fn get_all_tasks(&self) -> Vec<DownloadTask> {
        self.tasks
            .iter()
            .filter_map(|entry| {
                // Try to lock and clone - skip if currently locked
                entry.value().try_lock().ok().map(|task| task.clone())
            })
            .collect()
    }

    /// Get tasks filtered by state
    ///
    /// # Arguments
    ///
    /// * `state` - The state to filter by
    pub fn get_tasks_by_state(&self, state: TaskState) -> Vec<DownloadTask> {
        self.tasks
            .iter()
            .filter_map(|entry| {
                entry.value().try_lock().ok().and_then(|task| {
                    if task.state == state {
                        Some(task.clone())
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Update the state of a task
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task to update
    /// * `new_state` - The new state to set
    ///
    /// # Returns
    ///
    /// Returns Ok if the task was found and updated, Err otherwise
    pub async fn update_task_state(
        &self,
        task_id: &TaskId,
        new_state: TaskState,
    ) -> Result<(), Error> {
        if let Some(entry) = self.tasks.get(task_id) {
            let mut task = entry.value().lock().await;
            task.state = new_state;

            // Update timestamps based on state transitions
            match new_state {
                TaskState::Running if task.started_at.is_none() => {
                    task.started_at = Some(std::time::SystemTime::now());
                }
                TaskState::Completed | TaskState::Failed | TaskState::Cancelled => {
                    task.completed_at = Some(std::time::SystemTime::now());
                }
                _ => {}
            }

            tracing::debug!("Updated task {} state to {:?}", task_id, new_state);
            Ok(())
        } else {
            Err(Error::TaskNotFound(task_id.to_string()))
        }
    }

    /// Update task progress
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task
    /// * `progress` - The new progress information
    pub async fn update_task_progress(
        &self,
        task_id: &TaskId,
        progress: crate::download::types::ProgressUpdate,
    ) -> Result<(), Error> {
        if let Some(entry) = self.tasks.get(task_id) {
            let mut task = entry.value().lock().await;
            task.progress = progress;
            Ok(())
        } else {
            Err(Error::TaskNotFound(task_id.to_string()))
        }
    }

    /// Increment retry count for a task
    pub async fn increment_retry(&self, task_id: &TaskId) -> Result<u32, Error> {
        if let Some(entry) = self.tasks.get(task_id) {
            let mut task = entry.value().lock().await;
            task.retry_count += 1;
            Ok(task.retry_count)
        } else {
            Err(Error::TaskNotFound(task_id.to_string()))
        }
    }

    /// Set error message for a failed task
    pub async fn set_task_error(&self, task_id: &TaskId, error: String) -> Result<(), Error> {
        if let Some(entry) = self.tasks.get(task_id) {
            let mut task = entry.value().lock().await;
            task.error = Some(error);
            Ok(())
        } else {
            Err(Error::TaskNotFound(task_id.to_string()))
        }
    }

    /// Remove a task from the queue
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task to remove
    ///
    /// # Returns
    ///
    /// Returns Ok if the task was removed, Err if not found
    pub async fn remove_task(&self, task_id: &TaskId) -> Result<(), Error> {
        // Remove from tasks map
        if self.tasks.remove(task_id).is_none() {
            return Err(Error::TaskNotFound(task_id.to_string()));
        }

        // Also remove from pending queue if present
        let mut queue = self.pending_queue.lock().await;
        queue.retain(|id| id != task_id);

        tracing::debug!("Removed task {} from queue", task_id);
        Ok(())
    }

    /// Check if a task exists
    pub fn contains_task(&self, task_id: &TaskId) -> bool {
        self.tasks.contains_key(task_id)
    }

    /// Get queue statistics
    pub async fn stats(&self) -> crate::download::types::DownloadStats {
        let mut stats = crate::download::types::DownloadStats::default();

        for entry in self.tasks.iter() {
            if let Ok(task) = entry.value().try_lock() {
                stats.total_tasks += 1;
                match task.state {
                    TaskState::Pending => stats.pending += 1,
                    TaskState::Running => {
                        stats.running += 1;
                        stats.total_speed += task.progress.bytes_per_second;
                    }
                    TaskState::Pausing | TaskState::Paused => stats.paused += 1,
                    TaskState::Completed => {
                        stats.completed += 1;
                        stats.total_bytes_downloaded += task.progress.bytes_downloaded;
                    }
                    TaskState::Failed => stats.failed += 1,
                    TaskState::Cancelled => stats.cancelled += 1,
                }
            }
        }

        stats
    }

    /// Export queue state for persistence
    ///
    /// # Returns
    ///
    /// A serializable QueueState containing all tasks and configuration
    pub async fn export_state(&self) -> QueueState {
        let tasks = self.get_all_tasks();
        let config = self.config.read().await.clone();
        QueueState::new(tasks, config)
    }

    /// Import queue state from a previously exported state
    ///
    /// # Arguments
    ///
    /// * `state` - The QueueState to import
    ///
    /// # Returns
    ///
    /// Returns a vector of task IDs that were imported
    pub async fn import_state(&self, state: QueueState) -> Result<Vec<TaskId>, Error> {
        // Update configuration
        {
            let mut config = self.config.write().await;
            *config = state.config;
        }

        let mut imported_ids = Vec::new();

        for task in state.tasks {
            let task_state = task.state;
            let task_id = task.id;
            
            // Only import tasks that can be resumed
            if !task_state.is_terminal() || task_state == TaskState::Paused {
                // Add to tasks map
                self.tasks
                    .insert(task_id, Arc::new(Mutex::new(task)));

                // Add to pending queue if in pending state
                if task_state == TaskState::Pending {
                    let mut queue = self.pending_queue.lock().await;
                    queue.push_back(task_id);
                }

                imported_ids.push(task_id);
            }
        }

        tracing::info!("Imported {} tasks from state", imported_ids.len());
        Ok(imported_ids)
    }

    /// Export state to a file
    ///
    /// # Arguments
    ///
    /// * `path` - Path to save the state file
    pub async fn export_to_file(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        let state = self.export_state().await;
        let json = serde_json::to_string_pretty(&state)?;
        tokio::fs::write(path, json).await?;
        tracing::info!("Exported queue state to file");
        Ok(())
    }

    /// Import state from a file
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the state file
    pub async fn import_from_file(&self, path: impl AsRef<Path>) -> Result<Vec<TaskId>, Error> {
        let json = tokio::fs::read_to_string(path).await?;
        let state: QueueState = serde_json::from_str(&json)?;
        self.import_state(state).await
    }

    /// Get the current configuration
    pub async fn config(&self) -> DownloadConfig {
        self.config.read().await.clone()
    }

    /// Update the configuration
    pub async fn set_config(&self, config: DownloadConfig) {
        let mut c = self.config.write().await;
        *c = config;
    }

    /// Close the queue
    ///
    /// Once closed, no new tasks can be added. Existing tasks can still
    /// be processed by workers.
    pub fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        tracing::info!("Download queue closed");
    }

    /// Check if the queue is closed
    pub fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the number of pending tasks
    pub async fn pending_count(&self) -> usize {
        self.pending_queue.lock().await.len()
    }

    /// Get the total number of tasks
    pub fn total_count(&self) -> usize {
        self.tasks.len()
    }

    /// Clear all completed tasks from the queue
    pub async fn clear_completed(&self) -> usize {
        let mut removed = 0;
        let completed_ids: Vec<TaskId> = self
            .tasks
            .iter()
            .filter_map(|entry| {
                entry.value().try_lock().ok().and_then(|task| {
                    if task.state == TaskState::Completed {
                        Some(task.id)
                    } else {
                        None
                    }
                })
            })
            .collect();

        for task_id in completed_ids {
            if self.remove_task(&task_id).await.is_ok() {
                removed += 1;
            }
        }

        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use url::Url;

    fn create_test_task(id: &str, url: &str) -> DownloadTask {
        let config = DownloadConfig::default();
        let mut task = DownloadTask::new(
            Url::parse(url).unwrap(),
            PathBuf::from(format!("/tmp/{}.txt", id)),
            &config,
        );
        // Override the ID for testing
        task.id = TaskId(uuid::Uuid::parse_str(id).unwrap_or_else(|_| uuid::Uuid::new_v4()));
        task
    }

    #[tokio::test]
    async fn test_add_and_get_task() {
        let queue = DownloadQueue::new(DownloadConfig::default());
        let task = create_test_task("550e8400-e29b-41d4-a716-446655440000", "https://dummyimage.com/6000x4000/000/fff.png&text=test");
        let task_id = task.id;

        let added_id = queue.add_task(task).await.unwrap();
        assert_eq!(added_id, task_id);

        let retrieved = queue.get_task(&task_id).await.unwrap();
        let locked = retrieved.lock().await;
        assert_eq!(locked.id, task_id);
    }

    #[tokio::test]
    async fn test_next_pending() {
        let queue = DownloadQueue::new(DownloadConfig::default());
        
        // Add multiple tasks
        let task1 = create_test_task("550e8400-e29b-41d4-a716-446655440001", "https://dummyimage.com/6000x4000/000/fff.png&text=test");
        let task2 = create_test_task("550e8400-e29b-41d4-a716-446655440002", "https://dummyimage.com/6000x4000/000/fff.png&text=test2");
        
        queue.add_task(task1).await.unwrap();
        queue.add_task(task2).await.unwrap();

        // Get next pending
        let next = queue.next_pending().await;
        assert!(next.is_some());
        
        // Mark as running
        let task = next.unwrap();
        let task_id = task.lock().await.id;
        queue.update_task_state(&task_id, TaskState::Running).await.unwrap();

        // Get next pending again
        let next2 = queue.next_pending().await;
        assert!(next2.is_some());
    }

    #[tokio::test]
    async fn test_update_task_state() {
        let queue = DownloadQueue::new(DownloadConfig::default());
        let task = create_test_task("550e8400-e29b-41d4-a716-446655440003", "https://dummyimage.com/6000x4000/000/fff.png&text=test");
        let task_id = task.id;

        queue.add_task(task).await.unwrap();
        queue.update_task_state(&task_id, TaskState::Running).await.unwrap();

        let retrieved = queue.get_task(&task_id).await.unwrap();
        let locked = retrieved.lock().await;
        assert_eq!(locked.state, TaskState::Running);
    }

    #[tokio::test]
    async fn test_stats() {
        let queue = DownloadQueue::new(DownloadConfig::default());
        
        // Add tasks in different states
        let task1 = create_test_task("550e8400-e29b-41d4-a716-446655440004", "https://dummyimage.com/6000x4000/000/fff.png&text=test");
        let task2 = create_test_task("550e8400-e29b-41d4-a716-446655440005", "https://dummyimage.com/6000x4000/000/fff.png&text=test2");
        
        let id1 = task1.id;
        let id2 = task2.id;
        
        queue.add_task(task1).await.unwrap();
        queue.add_task(task2).await.unwrap();
        
        queue.update_task_state(&id1, TaskState::Completed).await.unwrap();

        let stats = queue.stats().await;
        assert_eq!(stats.total_tasks, 2);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.pending, 1);
    }

    #[tokio::test]
    async fn test_export_import_state() {
        let queue = DownloadQueue::new(DownloadConfig::default());
        let task = create_test_task("550e8400-e29b-41d4-a716-446655440006", "https://dummyimage.com/6000x4000/000/fff.png&text=test");
        let task_id = task.id;

        queue.add_task(task).await.unwrap();

        let state = queue.export_state().await;
        assert_eq!(state.tasks.len(), 1);

        let new_queue = DownloadQueue::new(DownloadConfig::default());
        let imported = new_queue.import_state(state).await.unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0], task_id);
    }

    #[tokio::test]
    async fn test_queue_close() {
        let queue = DownloadQueue::new(DownloadConfig::default());
        assert!(!queue.is_closed());

        queue.close();
        assert!(queue.is_closed());

        let task = create_test_task("550e8400-e29b-41d4-a716-446655440007", "https://dummyimage.com/6000x4000/000/fff.png&text=test");
        assert!(queue.add_task(task).await.is_err());
    }
}
