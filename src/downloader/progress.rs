use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    bytes_downloaded: u64,
    total_bytes: Option<u64>,
    speed_bps: Option<u64>,
    eta: Option<Duration>,

    // For calculations
    start_time: Instant,
    last_update: Instant,
    last_speed_update: Instant,
    last_bytes_for_speed: u64,
    update_interval: Duration,
}

impl std::fmt::Display for DownloadProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let percent = self.percent().unwrap_or(0.0);
        let speed = self
            .speed()
            .map(|s| format!("{:.2} B/s", s))
            .unwrap_or("N/A".to_string());
        let eta = self
            .eta
            .map(|d| format!("{:.2?}", d))
            .unwrap_or("N/A".to_string());
        let elapsed = self.elapsed();

        write!(
            f,
            "Downloaded: {} bytes, Total: {:?}, Speed: {}, ETA: {}, Elapsed: {:.2?}, Progress: {:.2}%",
            self.bytes_downloaded,
            self.total_bytes,
            speed,
            eta,
            elapsed,
            percent
        )
    }
}

impl DownloadProgress {
    pub fn new(bytes_downloaded: u64, total_bytes: Option<u64>, update_interval: Duration) -> Self {
        let now = Instant::now();
        Self {
            bytes_downloaded,
            total_bytes,
            speed_bps: None,
            eta: None,

            start_time: now,
            last_update: now,
            last_speed_update: now,
            last_bytes_for_speed: bytes_downloaded,
            update_interval,
        }
    }

    pub fn update(&self, bytes_downloaded: u64) -> Option<Self> {
        fn new_update(
            progress: &DownloadProgress,
            bytes_downloaded: u64,
            instant: Instant,
        ) -> DownloadProgress {
            DownloadProgress {
                eta: None,
                last_update: instant,
                bytes_downloaded,
                total_bytes: progress.total_bytes,
                speed_bps: progress.speed_bps,
                start_time: progress.start_time,
                last_speed_update: progress.last_speed_update,
                last_bytes_for_speed: progress.last_bytes_for_speed,
                update_interval: progress.update_interval,
            }
        }

        let now = Instant::now();

        if now.duration_since(self.last_update) < self.update_interval {
            return None;
        }
        let mut new_update = new_update(self, bytes_downloaded, now);

        if now.duration_since(self.last_speed_update) >= Duration::from_secs(1) {
            let byte_diff = (bytes_downloaded - self.last_bytes_for_speed) as f64;
            let time_diff = now.duration_since(self.last_speed_update).as_secs_f64();

            new_update.last_speed_update = now;
            new_update.last_bytes_for_speed = bytes_downloaded;
            new_update.speed_bps = Some((byte_diff / time_diff) as u64);
        };

        if let (Some(speed), Some(total)) = (new_update.speed_bps, self.total_bytes) {
            if speed > 0 {
                let remaining = total.saturating_sub(bytes_downloaded);
                new_update.eta = Some(Duration::from_secs(remaining / speed));
            }
        };

        Some(new_update)
    }

    pub fn percent(&self) -> Option<f64> {
        self.total_bytes.map(|total| {
            if total == 0 {
                0.0
            } else {
                (self.bytes_downloaded as f64 / total as f64) * 100.0
            }
        })
    }

    pub fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    pub fn bytes_downloaded(&self) -> u64 {
        self.bytes_downloaded
    }

    pub fn speed(&self) -> Option<u64> {
        self.speed_bps
    }

    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }
}
