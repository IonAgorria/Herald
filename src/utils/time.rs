use std::ops::Sub;
use std::time::{SystemTime, Duration, UNIX_EPOCH, Instant};

pub type Timestamp = (u64, u32);

pub fn duration_since_unix_epoch() -> Option<Duration> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
}

pub fn get_timestamp() -> Timestamp {
    duration_since_unix_epoch()
        .map_or((0, 0), |dur| (dur.as_secs(), dur.subsec_nanos()))
}

pub fn duration_since_timestamp(stamp: &Timestamp) -> Duration {
    duration_since_unix_epoch()
        .unwrap_or_default()
        .saturating_sub(Duration::from_nanos(stamp.1 as u64))
        .saturating_sub(Duration::from_secs(stamp.0))
}

#[derive(Debug, Clone)]
pub struct IntervalTimer {
    pub duration: Duration,
    pub last_tick: Instant,
}

impl IntervalTimer {
    pub fn new(duration: Duration) -> Self {
        Self {
            duration,
            last_tick: Instant::now().sub(duration),
        }
    }
    
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        if !self.duration.is_zero() && now.duration_since(self.last_tick) > self.duration {
            self.last_tick = now;
            true
        } else {
            false
        }
    }
}
