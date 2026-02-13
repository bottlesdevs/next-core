//! Real-time progress tracking and observables
//!
//! This module provides mechanisms for subscribing to download progress
//! updates in real-time. It uses tokio's watch and broadcast channels
//! to efficiently distribute updates to multiple subscribers.

use crate::download::types::{ProgressUpdate, TaskId};
use dashmap::DashMap;
use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::{broadcast, watch};

/// Progress tracker for managing real-time progress subscriptions
///
/// Maintains per-task watch channels and a global broadcast channel for
/// distributing progress updates to subscribers.
#[derive(Debug)]
pub struct ProgressTracker {
    /// Per-task progress watchers
    task_watchers: DashMap<TaskId, watch::Sender<ProgressUpdate>>,
    /// Global broadcast channel for all progress updates
    global_sender: broadcast::Sender<(TaskId, ProgressUpdate)>,
}

impl ProgressTracker {
    /// Create a new progress tracker
    ///
    /// # Arguments
    ///
    /// * `global_capacity` - The capacity of the global broadcast channel
    pub fn new(global_capacity: usize) -> Self {
        let (global_sender, _) = broadcast::channel(global_capacity);

        Self {
            task_watchers: DashMap::new(),
            global_sender,
        }
    }

    /// Subscribe to progress updates for a specific task
    ///
    /// Returns a watch receiver that will receive updates whenever the task's
    /// progress changes. This is a real-time observable - each update is sent
    /// as soon as it's available.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task to subscribe to
    ///
    /// # Returns
    ///
    /// A watch receiver for progress updates
    pub fn subscribe_task(&self, task_id: TaskId) -> watch::Receiver<ProgressUpdate> {
        // Get or create the watch sender for this task
        let sender = self
            .task_watchers
            .entry(task_id)
            .or_insert_with(|| {
                let (tx, _rx) = watch::channel(ProgressUpdate::default());
                tx
            })
            .value()
            .clone();

        sender.subscribe()
    }

    /// Subscribe to all progress updates
    ///
    /// Returns a broadcast receiver that will receive updates for all tasks.
    /// Updates are tuples of (TaskId, ProgressUpdate).
    ///
    /// # Returns
    ///
    /// A broadcast receiver for all progress updates
    pub fn subscribe_all(&self) -> broadcast::Receiver<(TaskId, ProgressUpdate)> {
        self.global_sender.subscribe()
    }

    /// Update progress for a task
    ///
    /// Sends the progress update to all subscribers of this task and
    /// broadcasts it to the global channel.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task being updated
    /// * `progress` - The new progress information
    pub fn update_progress(&self, task_id: TaskId, progress: ProgressUpdate) {
        // Update task-specific watcher
        if let Some(entry) = self.task_watchers.get(&task_id) {
            let sender = entry.value();
            let _ = sender.send(progress);
        } else {
            // Create watcher if it doesn't exist
            let (sender, _) = watch::channel(progress);
            self.task_watchers.insert(task_id, sender);
        }

        // Broadcast to global channel
        let _ = self.global_sender.send((task_id, progress));
    }

    /// Remove a task's progress tracker
    ///
    /// Called when a task is completed or removed from the queue.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task to remove
    pub fn remove_task(&self, task_id: &TaskId) {
        self.task_watchers.remove(task_id);
    }

    /// Get the current progress for a task
    ///
    /// Returns the most recent progress update for the task, if available.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task
    pub fn get_progress(&self, task_id: &TaskId) -> Option<ProgressUpdate> {
        self.task_watchers
            .get(task_id)
            .and_then(|entry| entry.value().borrow().clone().into())
    }

    /// Get the number of tracked tasks
    pub fn tracked_tasks(&self) -> usize {
        self.task_watchers.len()
    }
}

impl Clone for ProgressTracker {
    fn clone(&self) -> Self {
        // Create a new broadcast channel - receivers will need to resubscribe
        let (global_sender, _) = broadcast::channel(1024);
        
        Self {
            task_watchers: DashMap::new(), // Don't clone task watchers, they're recreated on demand
            global_sender,
        }
    }
}

/// Stream wrapper for progress updates
///
/// Converts a watch receiver into a Stream for use with async/await.
pub struct ProgressStream {
    inner: watch::Receiver<ProgressUpdate>,
}

impl ProgressStream {
    pub fn new(receiver: watch::Receiver<ProgressUpdate>) -> Self {
        Self { inner: receiver }
    }
}

impl Stream for ProgressStream {
    type Item = ProgressUpdate;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Check if there's a new value
        match self.inner.has_changed() {
            Ok(true) => {
                let progress = self.inner.borrow().clone();
                let _ = self.inner.changed();
                Poll::Ready(Some(progress))
            }
            Ok(false) => {
                // Register for wakeup when value changes
                let waker = cx.waker().clone();
                tokio::spawn(async move {
                    // This is a simplified approach - in production you might want
                    // a more efficient mechanism
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    waker.wake();
                });
                Poll::Pending
            }
            Err(_) => Poll::Ready(None), // Channel closed
        }
    }
}

/// Stream wrapper for global progress updates
pub struct GlobalProgressStream {
    inner: broadcast::Receiver<(TaskId, ProgressUpdate)>,
}

impl GlobalProgressStream {
    pub fn new(receiver: broadcast::Receiver<(TaskId, ProgressUpdate)>) -> Self {
        Self { inner: receiver }
    }
}

impl Stream for GlobalProgressStream {
    type Item = (TaskId, ProgressUpdate);

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.try_recv() {
            Ok(update) => Poll::Ready(Some(update)),
            Err(broadcast::error::TryRecvError::Empty) => Poll::Pending,
            Err(broadcast::error::TryRecvError::Closed) => Poll::Ready(None),
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                // We missed some updates, continue polling
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_progress_tracker_task_subscription() {
        let tracker = ProgressTracker::new(100);
        let task_id = TaskId::new();

        // Subscribe to task progress
        let mut rx = tracker.subscribe_task(task_id);
        
        // Initial value should be default
        assert_eq!(rx.borrow().bytes_downloaded, 0);

        // Update progress
        let progress = ProgressUpdate::new(1024, Some(2048));
        tracker.update_progress(task_id, progress);

        // Should receive the update
        rx.changed().await.unwrap();
        assert_eq!(rx.borrow().bytes_downloaded, 1024);
    }

    #[tokio::test]
    async fn test_progress_tracker_global_subscription() {
        let tracker = ProgressTracker::new(100);
        let task_id = TaskId::new();

        // Subscribe to all progress
        let mut rx = tracker.subscribe_all();

        // Update progress
        let progress = ProgressUpdate::new(2048, Some(4096));
        tracker.update_progress(task_id, progress);

        // Should receive on global channel
        let (received_id, received_progress) = rx.recv().await.unwrap();
        assert_eq!(received_id, task_id);
        assert_eq!(received_progress.bytes_downloaded, 2048);
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let tracker = ProgressTracker::new(100);
        let task_id = TaskId::new();

        // Multiple subscribers for same task
        let mut rx1 = tracker.subscribe_task(task_id);
        let mut rx2 = tracker.subscribe_task(task_id);

        // Update progress
        tracker.update_progress(task_id, ProgressUpdate::new(1000, Some(2000)));

        // Both should receive
        rx1.changed().await.unwrap();
        rx2.changed().await.unwrap();
        
        assert_eq!(rx1.borrow().bytes_downloaded, 1000);
        assert_eq!(rx2.borrow().bytes_downloaded, 1000);
    }

    #[test]
    fn test_get_progress() {
        let tracker = ProgressTracker::new(100);
        let task_id = TaskId::new();

        // Initially no progress
        assert!(tracker.get_progress(&task_id).is_none());

        // Update and get
        let progress = ProgressUpdate::new(500, Some(1000));
        tracker.update_progress(task_id, progress);

        let current = tracker.get_progress(&task_id).unwrap();
        assert_eq!(current.bytes_downloaded, 500);
    }

    #[tokio::test]
    async fn test_remove_task() {
        let tracker = ProgressTracker::new(100);
        let task_id = TaskId::new();

        tracker.update_progress(task_id, ProgressUpdate::new(100, Some(200)));
        assert!(tracker.get_progress(&task_id).is_some());

        tracker.remove_task(&task_id);
        assert!(tracker.get_progress(&task_id).is_none());
    }
}
