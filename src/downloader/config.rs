use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Debug, Clone)]
pub struct DownloadManagerConfig {
    max_concurrent: Arc<AtomicUsize>,
    queue_size: usize,
}

impl Default for DownloadManagerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: Arc::new(AtomicUsize::new(3)),
            queue_size: 100,
        }
    }
}

impl DownloadManagerConfig {
    pub fn queue_size(&self) -> usize {
        self.queue_size
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent.load(Ordering::Relaxed)
    }

    pub fn set_max_concurrent(&self, max: usize) {
        self.max_concurrent.store(max, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub struct DownloadConfig {
    max_retries: usize,
    user_agent: Option<String>,
    progress_update_interval: Duration,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            user_agent: None,
            progress_update_interval: Duration::from_millis(1000),
        }
    }
}

impl DownloadConfig {
    pub fn max_retries(&self) -> usize {
        self.max_retries
    }

    pub fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }

    pub fn progress_update_interval(&self) -> Duration {
        self.progress_update_interval
    }
}
