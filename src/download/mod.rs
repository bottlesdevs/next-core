//! Download Manager Module
//!
//! A comprehensive download manager with queue management, concurrent workers,
//! pause/resume support, progress tracking, and crash recovery.
//!
//! # Features
//!
//! - **Queue Management**: FIFO queue with thread-safe operations
//! - **Concurrent Downloads**: Configurable worker pool with semaphore-based concurrency
//! - **Pause/Resume**: Graceful pause after current chunk with resume support
//! - **Progress Tracking**: Real-time progress observables using tokio channels
//! - **Retry Logic**: Exponential backoff with jitter for failed downloads
//! - **State Persistence**: Export/import queue state for crash recovery
//! - **Event Hooks**: Subscribe to state changes and progress updates
//! - **Custom Backends**: Pluggable HTTP backend support
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use bottles_core::download::{DownloadManager, DownloadConfig};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create manager
//!     let manager = DownloadManager::new(DownloadConfig::default())?;
//!
//!     // Add downloads
//!     let task_id = manager.add_url("https://dummyimage.com/6000x4000/000/fff.png&text=test").await?;
//!
//!     // Subscribe to progress
//!     let mut progress = manager.subscribe_progress(task_id);
//!     tokio::spawn(async move {
//!         while progress.changed().await.is_ok() {
//!             let p = progress.borrow();
//!             println!("Progress: {:.1}%", p.percentage.unwrap_or(0.0));
//!         }
//!     });
//!
//!     // Wait for completion
//!     manager.wait_for_all().await;
//!
//!     Ok(())
//! }
//! ```

// Core types
pub mod types;

// Backend abstraction
pub mod backend;

// Queue management
pub mod queue;

// Progress tracking
pub mod progress;

// Event system
pub mod events;

// Retry logic
pub mod retry;

// Worker pool
pub mod worker;

// Main manager
pub mod manager;

// Re-export commonly used types
pub use types::{
    DownloadConfig,
    DownloadStats,
    DownloadTask,
    ProgressUpdate,
    QueueState,
    StateChangeEvent,
    TaskId,
    TaskState,
};

pub use backend::{DownloadBackend, ReqwestBackend};

pub use queue::DownloadQueue;

pub use progress::ProgressTracker;

pub use events::EventManager;

pub use retry::RetryPolicy;

pub use manager::DownloadManager;

// Internal modules (not publicly exposed)
// mod worker; - worker is implementation detail
