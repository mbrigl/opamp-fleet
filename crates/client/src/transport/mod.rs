//! The two OpAMP transports (ADR-0007). Both feed the same [`Agent`](crate::agent::Agent) state
//! machine; they differ only in how bytes travel.

pub mod http;
pub mod ws;

use std::time::Duration;

/// Reconnect backoff: exponential from one second, capped at a minute.
pub struct Backoff {
    next: Duration,
}

impl Backoff {
    const START: Duration = Duration::from_secs(1);
    const CAP: Duration = Duration::from_secs(60);

    pub fn new() -> Self {
        Backoff { next: Self::START }
    }

    pub fn reset(&mut self) {
        self.next = Self::START;
    }

    /// The delay to wait now; subsequent failures wait longer.
    pub fn advance(&mut self) -> Duration {
        let current = self.next;
        self.next = (self.next * 2).min(Self::CAP);
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let mut backoff = Backoff::new();
        assert_eq!(backoff.advance(), Duration::from_secs(1));
        assert_eq!(backoff.advance(), Duration::from_secs(2));
        for _ in 0..10 {
            backoff.advance();
        }
        assert_eq!(backoff.advance(), Duration::from_secs(60));
        backoff.reset();
        assert_eq!(backoff.advance(), Duration::from_secs(1));
    }
}
