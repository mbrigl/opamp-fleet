//! The two OpAMP transports (ADR-0023). Both feed the same [`Agent`](crate::agent::Agent) state
//! machine; they differ only in how bytes travel.

pub mod http;
pub mod ws;

use std::time::Duration;

use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

use crate::config::ClientConfig;
use crate::engine::Engine;
/// The close the Baseline names for a message past the size limit: 1009, Message Too Big.
///
/// Both sockets this Client owns send it — the upstream one it dials and the Supervisor Endpoint it
/// serves — and they sent identical copies of it until ADR-0025. The sentence itself is the
/// Server's too, and lives in `opamp::frame`.
pub(crate) fn too_big_close() -> CloseFrame {
    CloseFrame {
        code: CloseCode::Size,
        reason: opamp::frame::TOO_BIG_CLOSE_REASON.into(),
    }
}

///
/// `pub(crate)` deliberately: nothing outside this crate has a use for it, and ADR-0025 widens
/// visibility by need rather than by default. Left `pub` it would be a public type whose `new`
/// takes no arguments, which is a `Default` this crate would then have to keep meaning something.
pub(crate) struct Backoff {
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

    pub fn advance(&mut self) -> Duration {
        self.next = (self.next * 2).min(Self::CAP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use crate::supervisor::agent::AgentState;

    #[test]
        let mut backoff = Backoff::new();
        for _ in 0..10 {
            backoff.advance();
        }
        backoff.reset();
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
    }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
            }
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
}
