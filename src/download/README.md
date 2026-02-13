# Download Manager

A comprehensive, async download manager for Rust with support for concurrent downloads, pause/resume, progress tracking, and crash recovery.

## Features

- **Queue Management**: Thread-safe FIFO queue for download tasks
- **Concurrent Downloads**: Configurable worker pool with semaphore-based concurrency control
- **Pause/Resume**: Graceful pause after current chunk completes, with resume support from partial downloads
- **Progress Tracking**: Real-time progress observables using tokio channels (no debouncing)
- **Retry Logic**: Exponential backoff with jitter for transient failures
- **State Persistence**: Export/import queue state for crash recovery
- **Event System**: Subscribe to state changes with callbacks or async streams
- **Custom Backends**: Pluggable HTTP backend trait (includes Reqwest implementation)
- **Atomic Operations**: Temp files with atomic move on completion
- **HTTP Range Support**: Resume interrupted downloads from where they left off

## Quick Start

```rust
use bottles_core::download::{DownloadManager, DownloadConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create manager with default configuration
    let manager = DownloadManager::new(DownloadConfig::default())?;

    // Add a download
    let task_id = manager.add_url("https://dummyimage.com/6000x4000/000/fff.png&text=test").await?;

    // Subscribe to progress updates (real-time, no debouncing)
    let mut progress = manager.subscribe_progress(task_id);
    tokio::spawn(async move {
        while progress.changed().await.is_ok() {
            let p = *progress.borrow();
            println!("Progress: {:.1}%", p.percentage.unwrap_or(0.0));
        }
    });

    // Wait for completion
    manager.wait_for_all().await;
    
    Ok(())
}
```

## Architecture

The download manager is built around several key components:

### DownloadManager
The main interface that orchestrates all operations. It's cheap to clone (`Arc`-backed) and thread-safe.

### DownloadQueue
Thread-safe FIFO queue using `DashMap` for O(1) lookups and `tokio::sync::Mutex` for queue ordering.

### WorkerPool
Manages concurrent downloads using a semaphore to limit concurrent workers. Each worker processes one download at a time.

### DownloadBackend
Trait-based abstraction for HTTP operations. Default implementation uses `reqwest` with support for:
- HTTP/HTTPS
- Range requests for resume
- Custom headers
- Streaming response body

### ProgressTracker
Real-time progress distribution using:
- `tokio::sync::watch` for per-task progress (single-producer, multi-consumer)
- `tokio::sync::broadcast` for global progress updates

### EventManager
State change notifications using broadcast channels. Supports both callback functions and async subscriptions.

## Configuration

```rust
use bottles_core::download::DownloadConfig;
use std::path::PathBuf;

let config = DownloadConfig {
    // Maximum concurrent downloads
    max_concurrent_downloads: 4,
    
    // Default directory for downloads
    default_download_dir: PathBuf::from("./downloads"),
    
    // Retry configuration
    max_retries: 3,
    retry_delay_ms: 1000,      // Initial retry delay
    max_retry_delay_ms: 60000, // Cap at 60 seconds
    
    // Download settings
    chunk_size: 8192, // 8KB chunks for progress updates
    
    // Auto-start workers when first task is added
    auto_start: true,
};
```

## Pause and Resume

The manager supports graceful pause/resume with HTTP Range headers:

```rust
// Pause a task (waits for current chunk to complete)
manager.pause_task(task_id).await?;

// Resume from partial download
manager.resume_task(task_id).await?;

// Pause/resume all active tasks
manager.pause_all().await;
manager.resume_all().await;
```

When pausing, a `Pausing` state event is emitted to notify that the task will pause after the current chunk completes. This prevents file corruption from partial writes.

## Progress Tracking

Progress updates are sent in real-time without debouncing:

```rust
// Per-task progress
let mut progress_rx = manager.subscribe_progress(task_id);

// All tasks progress
let mut all_progress = manager.subscribe_all_progress();

// Progress includes:
// - bytes_downloaded
// - total_bytes (if known from Content-Length)
// - bytes_per_second (current speed)
// - eta_seconds (estimated time remaining)
// - percentage (0.0 to 100.0)
```

## Event System

Subscribe to state changes:

```rust
// Using callbacks
manager.on_state_change(|event| {
    println!("Task {}: {:?} -> {:?}", 
        event.task_id, 
        event.old_state, 
        event.new_state
    );
}).await;

// Using async streams
let mut events = manager.subscribe_state_changes();
while let Ok(event) = events.recv().await {
    // Handle event
}
```

## State Persistence

Export and import queue state for crash recovery:

```rust
// Export current state
manager.export_state("./downloads/state.json").await?;

// Later, after restart...
let manager = DownloadManager::new(config)?;
let imported_count = manager.import_state("./downloads/state.json").await?;
manager.start().await?;
```

The state includes:
- All non-terminal tasks (pending, running, paused)
- Download progress and partial file positions
- Configuration
- Retry counts

## Custom Backends

Implement your own HTTP backend:

```rust
use bottles_core::download::backend::DownloadBackend;
use async_trait::async_trait;

#[derive(Debug)]
struct MyBackend;

#[async_trait]
impl DownloadBackend for MyBackend {
    async fn download<F>(
        &self,
        task: &DownloadTask,
        resume_from: u64,
        on_progress: F,
    ) -> Result<u64, Error>
    where
        F: FnMut(ProgressUpdate) + Send,
    {
        // Your download implementation
    }

    async fn supports_resume(&self, task: &DownloadTask) -> Result<bool, Error> {
        // Check server capabilities
    }

    async fn get_content_length(&self, url: &Url) -> Result<Option<u64>, Error> {
        // Get file size
    }
}

// Use custom backend
let manager = DownloadManager::with_backend(config, Box::new(MyBackend));
```

## Error Handling

The manager distinguishes between:
- **Permanent errors** (4xx HTTP status, invalid URLs) - not retried
- **Transient errors** (network issues, 5xx status) - retried with exponential backoff

All errors are wrapped in the crate's `Error` type with context.

## Examples

See the `examples/` directory for:
- `basic_usage.rs` - Basic download with progress tracking
- `pause_resume.rs` - Pause/resume functionality
- `crash_recovery.rs` - State persistence and recovery

## Thread Safety

All components are thread-safe and can be used concurrently:
- `DownloadManager` uses `Arc<InnerManager>` internally
- Queue operations use `DashMap` and `tokio::sync` primitives
- Workers are spawned as separate Tokio tasks

Multiple clones of the manager can be used from different tasks to:
- Add downloads from different sources
- Control downloads (pause/resume/cancel)
- Monitor progress independently

