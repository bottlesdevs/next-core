//! Basic Download Manager Example
//!
//! This example demonstrates how to use the download manager to download
//! files with progress tracking, pause/resume, and event handling.

use bottles_core::download::{DownloadConfig, DownloadManager, TaskState};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for logging
    tracing_subscriber::fmt::init();

    // Create download configuration
    let config = DownloadConfig {
        max_concurrent_downloads: 3,
        default_download_dir: PathBuf::from("./downloads"),
        max_retries: 3,
        retry_delay_ms: 1000,
        max_retry_delay_ms: 30000,
        chunk_size: 8192,
        auto_start: true,
    };

    // Create the download manager
    let manager = DownloadManager::new(config)?;

    // Subscribe to state change events
    manager
        .on_state_change(|event| {
            println!(
                "Task {}: {} -> {}",
                event.task_id,
                event.old_state,
                event.new_state
            );

            // Special notification for pausing state
            if event.new_state == TaskState::Pausing {
                println!("  Task will pause after current chunk completes");
            }
        })
        .await;

    // Add some downloads
    let urls = vec![
        "https://dummyimage.com/6000x4000/000/fff.png&text=test",
        "https://dummyimage.com/6000x4000/000/fff.png&text=test2",
        "https://dummyimage.com/6000x4000/000/fff.png&text=test3",
    ];

    let mut task_ids = Vec::new();
    for url in urls {
        let task_id = manager.add_url(url).await?;
        println!("Added task {} for {}", task_id, url);
        task_ids.push(task_id);

        // Subscribe to progress for this task
        let mut progress_rx = manager.subscribe_progress(task_id);
        tokio::spawn(async move {
            while progress_rx.changed().await.is_ok() {
                let progress = progress_rx.borrow();
                if let Some(percentage) = progress.percentage {
                    println!(
                        "Task {}: {:.1}% ({}/{} bytes, {:.1} KB/s)",
                        task_id,
                        percentage,
                        progress.bytes_downloaded,
                        progress.total_bytes.unwrap_or(0),
                        progress.bytes_per_second / 1024.0
                    );
                }
            }
        });
    }

    // Subscribe to all progress updates
    let mut all_progress = manager.subscribe_all_progress();
    tokio::spawn(async move {
        while let Ok((task_id, progress)) = all_progress.recv().await {
            // Handle global progress updates
            if let Some(eta) = progress.eta_seconds {
                println!("Task {} ETA: {}s", task_id, eta);
            }
        }
    });

    // Let downloads run for a bit
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Demonstrate pause functionality
    if let Some(&task_id) = task_ids.first() {
        println!("\nPausing task {}", task_id);
        manager.pause_task(task_id).await?;

        // Wait a moment to see the Pausing -> Paused transition
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Resume the task
        println!("Resuming task {}\n", task_id);
        manager.resume_task(task_id).await?;
    }

    // Wait for all downloads to complete
    println!("Waiting for all downloads to complete...");
    manager.wait_for_all().await;

    // Print final statistics
    let stats = manager.stats().await;
    println!("\n=== Final Statistics ===");
    println!("Total tasks: {}", stats.total_tasks);
    println!("Completed: {}", stats.completed);
    println!("Failed: {}", stats.failed);
    println!("Cancelled: {}", stats.cancelled);
    println!("Total downloaded: {} bytes", stats.total_bytes_downloaded);

    // Export queue state for crash recovery
    manager.export_state("./downloads/queue_state.json").await?;
    println!("\nQueue state exported to ./downloads/queue_state.json");

    // Shutdown the manager
    manager.stop().await;

    Ok(())
}
