use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
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
