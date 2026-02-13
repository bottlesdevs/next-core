//! Event hooks and callback system for download state changes
//!
//! This module provides a flexible event system for subscribing to
//! download state changes, including pause notifications. Events are
//! delivered in real-time to all registered subscribers.

use crate::download::types::{StateChangeEvent, TaskId, TaskState};
use futures::Stream;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::{broadcast, Mutex};

/// Type alias for state change callback functions
pub type StateChangeCallback = Arc<dyn Fn(StateChangeEvent) + Send + Sync>;

/// Event manager for download state changes
///
/// Manages subscriptions to state change events and delivers them
/// to all registered listeners. Supports both callback functions
/// and async stream-based subscriptions.
pub struct EventManager {
    /// Broadcast channel for state change events
    sender: broadcast::Sender<StateChangeEvent>,
    /// Registered callback functions
    callbacks: Arc<Mutex<Vec<StateChangeCallback>>>,
}

impl std::fmt::Debug for EventManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventManager")
            .field("sender", &self.sender)
            .finish_non_exhaustive()
    }
}

impl EventManager {
    /// Create a new event manager
    ///
    /// # Arguments
    ///
    /// * `capacity` - The capacity of the broadcast channel
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);

        Self {
            sender,
            callbacks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Subscribe to state change events
    ///
    /// Returns a broadcast receiver that will receive all state change events.
    /// Use this for async/await style event handling.
    ///
    /// # Returns
    ///
    /// A broadcast receiver for state change events
    pub fn subscribe(&self) -> broadcast::Receiver<StateChangeEvent> {
        self.sender.subscribe()
    }

    /// Register a callback for state change events
    ///
    /// The callback will be called synchronously whenever a state change occurs.
    /// Use this for simple event handling or logging.
    ///
    /// # Arguments
    ///
    /// * `callback` - The callback function to register
    pub async fn on_state_change<F>(&self, callback: F)
    where
        F: Fn(StateChangeEvent) + Send + Sync + 'static,
    {
        let mut callbacks = self.callbacks.lock().await;
        callbacks.push(Arc::new(callback));
    }

    /// Emit a state change event
    ///
    /// Sends the event to all subscribers and callbacks.
    ///
    /// # Arguments
    ///
    /// * `event` - The state change event to emit
    pub async fn emit(&self, event: StateChangeEvent) {
        // Send to broadcast channel
        let _ = self.sender.send(event.clone());

        // Call registered callbacks
        let callbacks = self.callbacks.lock().await;
        for callback in callbacks.iter() {
            callback(event.clone());
        }
    }

    /// Emit a state change by providing old and new states
    ///
    /// Convenience method to create and emit a StateChangeEvent.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The ID of the task that changed state
    /// * `old_state` - The previous state
    /// * `new_state` - The new state
    pub async fn emit_state_change(
        &self,
        task_id: TaskId,
        old_state: TaskState,
        new_state: TaskState,
    ) {
        let event = StateChangeEvent::new(task_id, old_state, new_state);
        self.emit(event).await;
    }

    /// Remove all registered callbacks
    pub async fn clear_callbacks(&self) {
        let mut callbacks = self.callbacks.lock().await;
        callbacks.clear();
    }

    /// Get the number of active subscribers
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Clone for EventManager {
    fn clone(&self) -> Self {
        let (sender, _) = broadcast::channel(1024);
        
        Self {
            sender,
            callbacks: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// Stream wrapper for state change events
pub struct StateChangeStream {
    inner: broadcast::Receiver<StateChangeEvent>,
}

impl StateChangeStream {
    pub fn new(receiver: broadcast::Receiver<StateChangeEvent>) -> Self {
        Self { inner: receiver }
    }
}

impl Stream for StateChangeStream {
    type Item = StateChangeEvent;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.try_recv() {
            Ok(event) => Poll::Ready(Some(event)),
            Err(broadcast::error::TryRecvError::Empty) => Poll::Pending,
            Err(broadcast::error::TryRecvError::Closed) => Poll::Ready(None),
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                tracing::warn!("Event subscriber lagged behind by {} messages", skipped);
                Poll::Pending
            }
        }
    }
}

/// Builder for creating event handlers with specific filters
pub struct EventHandlerBuilder {
    event_manager: Arc<EventManager>,
    task_filter: Option<TaskId>,
    state_filter: Option<Vec<TaskState>>,
}

impl EventHandlerBuilder {
    /// Create a new event handler builder
    pub fn new(event_manager: Arc<EventManager>) -> Self {
        Self {
            event_manager,
            task_filter: None,
            state_filter: None,
        }
    }

    /// Filter events for a specific task only
    pub fn for_task(mut self, task_id: TaskId) -> Self {
        self.task_filter = Some(task_id);
        self
    }

    /// Filter events for specific states only
    pub fn for_states(mut self, states: Vec<TaskState>) -> Self {
        self.state_filter = Some(states);
        self
    }

    /// Build and register the handler
    pub async fn on_event<F>(self, callback: F)
    where
        F: Fn(StateChangeEvent) + Send + Sync + 'static,
    {
        let task_filter = self.task_filter;
        let state_filter = self.state_filter;

        let filtered_callback = move |event: StateChangeEvent| {
            // Apply task filter
            if let Some(task_id) = task_filter {
                if event.task_id != task_id {
                    return;
                }
            }

            // Apply state filter
            if let Some(ref states) = state_filter {
                if !states.contains(&event.new_state) {
                    return;
                }
            }

            callback(event);
        };

        self.event_manager.on_state_change(filtered_callback).await;
    }
}

/// Helper functions for common event patterns
pub mod helpers {
    use super::*;

    /// Create a handler that triggers when a task reaches a terminal state
    pub async fn on_complete<F>(event_manager: &EventManager, callback: F)
    where
        F: Fn(TaskId, TaskState) + Send + Sync + 'static,
    {
        event_manager
            .on_state_change(move |event| {
                if event.new_state.is_terminal() {
                    callback(event.task_id, event.new_state);
                }
            })
            .await;
    }

    /// Create a handler that triggers when a task is paused
    pub async fn on_paused<F>(event_manager: &EventManager, callback: F)
    where
        F: Fn(TaskId) + Send + Sync + 'static,
    {
        event_manager
            .on_state_change(move |event| {
                if event.new_state == TaskState::Paused {
                    callback(event.task_id);
                }
            })
            .await;
    }

    /// Create a handler that triggers when a task starts pausing
    /// This notifies that the task will pause after the current chunk
    pub async fn on_pausing<F>(event_manager: &EventManager, callback: F)
    where
        F: Fn(TaskId) + Send + Sync + 'static,
    {
        event_manager
            .on_state_change(move |event| {
                if event.new_state == TaskState::Pausing {
                    callback(event.task_id);
                }
            })
            .await;
    }

    /// Create a handler that triggers when a task starts downloading
    pub async fn on_started<F>(event_manager: &EventManager, callback: F)
    where
        F: Fn(TaskId) + Send + Sync + 'static,
    {
        event_manager
            .on_state_change(move |event| {
                if event.new_state == TaskState::Running && event.old_state == TaskState::Pending {
                    callback(event.task_id);
                }
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_subscription() {
        let manager = EventManager::new(100);
        let mut rx = manager.subscribe();

        let task_id = TaskId::new();
        let event = StateChangeEvent::new(task_id, TaskState::Pending, TaskState::Running);

        manager.emit(event.clone()).await;

        let received = rx.recv().await.unwrap();
        assert_eq!(received.task_id, task_id);
        assert_eq!(received.old_state, TaskState::Pending);
        assert_eq!(received.new_state, TaskState::Running);
    }

    #[tokio::test]
    async fn test_callback_registration() {
        let manager = EventManager::new(100);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        manager
            .on_state_change(move |event| {
                let _ = tx.send(event);
            })
            .await;

        let task_id = TaskId::new();
        let event = StateChangeEvent::new(task_id, TaskState::Running, TaskState::Paused);
        manager.emit(event).await;

        // Wait for the callback to send the event
        let received = rx.recv().await.unwrap();
        assert_eq!(received.new_state, TaskState::Paused);
    }

    #[tokio::test]
    async fn test_emit_state_change() {
        let manager = EventManager::new(100);
        let mut rx = manager.subscribe();

        let task_id = TaskId::new();
        manager
            .emit_state_change(task_id, TaskState::Running, TaskState::Pausing)
            .await;

        let received = rx.recv().await.unwrap();
        assert_eq!(received.new_state, TaskState::Pausing);
    }

    #[tokio::test]
    async fn test_helper_on_pausing() {
        let manager = EventManager::new(100);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        helpers::on_pausing(&manager, move |_task_id| {
            let _ = tx.send(true);
        })
        .await;

        let task_id = TaskId::new();
        manager
            .emit_state_change(task_id, TaskState::Running, TaskState::Pausing)
            .await;

        // Wait for the callback to send the signal
        let triggered = rx.recv().await.unwrap();
        assert!(triggered);
    }
}
