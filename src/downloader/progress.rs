use std::time::{Duration, Instant};

const SPEED_UPDATE_INTERVAL: Duration = Duration::from_secs(1);

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
    pub fn new(bytes_downloaded: u64, total_bytes: Option<u64>) -> Self {
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
        }
    }

    pub fn update(&mut self, bytes_downloaded: u64) {
        self.bytes_downloaded = bytes_downloaded;
        self.update_speed(bytes_downloaded);
        self.update_eta();
    }

    fn update_speed(&mut self, bytes_downloaded: u64) {
        let now = Instant::now();

        if now.duration_since(self.last_speed_update) >= SPEED_UPDATE_INTERVAL {
            let byte_diff = (bytes_downloaded - self.last_bytes_for_speed) as f64;
            let time_diff = now.duration_since(self.last_speed_update).as_secs_f64();

            self.last_speed_update = now;
            self.last_bytes_for_speed = bytes_downloaded;
            self.speed_bps = Some((byte_diff / time_diff) as u64);
        };
    }

    fn update_eta(&mut self) {
        if let (Some(speed), Some(total)) = (self.speed_bps, self.total_bytes) {
            if speed > 0 {
                let remaining = total.saturating_sub(self.bytes_downloaded);
                self.eta = Some(Duration::from_secs(remaining / speed));
            }
        }
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
