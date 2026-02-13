//! Pause and Resume Example
//!
//! This example demonstrates the pause and resume functionality of the download manager,
//! including the graceful pause after current chunk behavior.

use bottles_core::download::{DownloadConfig, DownloadManager, TaskState};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = DownloadConfig {
        max_concurrent_downloads: 2,
        default_download_dir: PathBuf::from("./downloads"),
        ..Default::default()
    };

    let manager = DownloadManager::new(config)?;

    // Subscribe to state changes to observe pause behavior
    let manager_clone = manager.clone();
    manager
        .on_state_change(move |event| {
            let manager = manager_clone.clone();
            tokio::spawn(async move {
                match event.new_state {
                    TaskState::Pausing => {
                        println!(
                            "🟡 Task {} is PAUSING (will pause after current chunk)",
                            event.task_id
                        );
                    }
                    TaskState::Paused => {
                        println!("🔴 Task {} is PAUSED", event.task_id);
                        
                        // Show resume capability
                        if let Some(task) = manager.get_task(event.task_id).await {
                            let partial_size = task.bytes_already_downloaded();
                            println!("  Can resume from {} bytes", partial_size);
                        }
                    }
                    TaskState::Running => {
                        println!("🟢 Task {} is RUNNING", event.task_id);
                    }
                    TaskState::Completed => {
                        println!("✅ Task {} COMPLETED", event.task_id);
                    }
                    _ => {}
                }
            });
        })
        .await;

    // Add a large download
    println!("Adding large file download...");
    let task_id = manager
        .add_url("https://dummyimage.com/6000x4000/000/fff.png&text=test")
        .await?;

    // Monitor progress
    let mut progress_rx = manager.subscribe_progress(task_id);
    let progress_task = tokio::spawn(async move {
        while progress_rx.changed().await.is_ok() {
            let progress = progress_rx.borrow();
            println!(
                "Progress: {:.1}% ({}/{})",
                progress.percentage.unwrap_or(0.0),
                progress.bytes_downloaded,
                progress.total_bytes.unwrap_or(0)
            );
        }
    });

    // Let it run for 3 seconds
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Pause the download
    println!("\n⏸️  Requesting pause...");
    manager.pause_task(task_id).await?;

    // Wait for it to actually pause
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Resume the download
    println!("\n▶️  Resuming download...");
    manager.resume_task(task_id).await?;

    // Let it continue
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Demonstrate pause all
    println!("\n⏸️  Pausing all downloads...");
    let paused_count = manager.pause_all().await;
    println!("Paused {} downloads", paused_count);

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Resume all
    println!("\n▶️  Resuming all downloads...");
    let resumed_count = manager.resume_all().await;
    println!("Resumed {} downloads", resumed_count);

    // Wait for completion
    manager.wait_for_all().await;

    // Cleanup
    drop(progress_task);
    manager.stop().await;

    Ok(())
}
