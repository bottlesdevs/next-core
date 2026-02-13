//! Crash Recovery Example
//!
//! This example demonstrates how to use the state persistence feature
//! for crash recovery. It shows how to:
//! 1. Export queue state
//! 2. Simulate a crash/restart
//! 3. Import the state and resume downloads

use bottles_core::download::{DownloadConfig, DownloadManager, TaskState};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let state_file = "./downloads/queue_state_backup.json";

    // Phase 1: Initial run - start some downloads and export state
    println!("=== Phase 1: Initial Run ===");
    {
        let config = DownloadConfig {
            default_download_dir: PathBuf::from("./downloads"),
            auto_start: true,
            ..Default::default()
        };

        let manager = DownloadManager::new(config)?;

        // Add downloads
        let urls = vec![
            "https://dummyimage.com/6000x4000/000/fff.png&text=test1",
            "https://dummyimage.com/6000x4000/000/fff.png&text=test2",
            "https://dummyimage.com/6000x4000/000/fff.png&text=test3",
        ];

        for url in urls {
            let task_id = manager.add_url(url).await?;
            println!("Started download: {} (task {})", url, task_id);
        }

        // Let downloads run for a bit
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Pause all to ensure clean state
        let paused = manager.pause_all().await;
        println!("\nPaused {} downloads", paused);

        // Export state before "crash"
        tokio::fs::create_dir_all("./downloads").await.ok();
        manager.export_state(state_file).await?;
        println!("State exported to {}", state_file);

        // Show what's in the state
        let stats = manager.stats().await;
        println!("\nQueue state before export:");
        println!("  Total: {}", stats.total_tasks);
        println!("  Pending: {}", stats.pending);
        println!("  Paused: {}", stats.paused);

        // "Crash" - manager goes out of scope
        println!("\n💥 Simulating crash...");
    }

    tokio::time::sleep(Duration::from_secs(1)).await;

    // Phase 2: Recovery - import state and resume
    println!("\n=== Phase 2: Recovery ===");
    {
        let config = DownloadConfig {
            default_download_dir: PathBuf::from("./downloads"),
            auto_start: false, // Don't auto-start, we'll import first
            ..Default::default()
        };

        let manager = DownloadManager::new(config)?;

        // Import the saved state
        let imported_count = manager.import_state(state_file).await?;
        println!("Imported {} tasks from state file", imported_count);

        // Show recovered tasks
        let tasks = manager.get_all_tasks();
        println!("\nRecovered tasks:");
        for task in tasks {
            let partial_bytes = task.bytes_already_downloaded();
            println!(
                "  {} - {} (state: {}, partial: {} bytes)",
                task.id,
                task.url,
                task.state,
                partial_bytes
            );
        }

        // Subscribe to progress
        for task in manager.get_all_tasks() {
            let mut progress_rx = manager.subscribe_progress(task.id);
            tokio::spawn(async move {
                while progress_rx.changed().await.is_ok() {
                    let progress = progress_rx.borrow();
                    if progress.percentage.map(|p| p >= 100.0).unwrap_or(false) {
                        break;
                    }
                }
            });
        }

        // Start the manager to resume downloads
        println!("\n▶️  Resuming downloads...");
        manager.start().await?;

        // Resume all paused tasks
        let resumed = manager.resume_all().await;
        println!("Resumed {} tasks", resumed);

        // Wait for completion (or timeout for demo)
        println!("\nWaiting for downloads to complete (30s timeout for demo)...");
        tokio::select! {
            _ = manager.wait_for_all() => {
                println!("All downloads completed!");
            }
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                println!("Timeout reached");
            }
        }

        // Final stats
        let stats = manager.stats().await;
        println!("\nFinal statistics:");
        println!("  Completed: {}", stats.completed);
        println!("  Failed: {}", stats.failed);
        println!("  Total bytes downloaded: {}", stats.total_bytes_downloaded);

        manager.stop().await;
    }

    // Cleanup
    tokio::fs::remove_file(state_file).await.ok();

    Ok(())
}
