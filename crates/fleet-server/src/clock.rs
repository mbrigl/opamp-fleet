//! The system clock: the adapter behind [`Clock`](crate::fleet::Clock).

use std::time::{SystemTime, UNIX_EPOCH};

use crate::fleet::Clock;

/// The host's wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

pub use crate::fleet::Elapsed;

impl Elapsed {
    /// `std::time::Instant` since its first reading: a suspend counts on some systems and not on
    /// others, on Linux not.
    #[must_use]
    pub fn monotonic() -> Self {
        static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        Elapsed::new(|| ORIGIN.get_or_init(std::time::Instant::now).elapsed())
    }
}
