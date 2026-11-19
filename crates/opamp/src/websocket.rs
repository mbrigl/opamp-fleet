//! The configuration of every WebSocket the Server's endpoint and the Client's socket open, with
//! `tungstenite` (DEV-six-crates-tokio-axum, SEC-tls13-two-planes-bounded-input): one place, so the two ends cannot drift apart.

use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// What each WebSocket reads from its stream at a time, and the read buffer it holds while
/// nothing arrives.
///
/// `tungstenite` allocates the read buffer at this size for every connection when the socket is
/// built, and its default is 128 KiB. An Agent's routine message is a few dozen bytes, and its
/// larger reports a few KiB, so 4 KiB holds most of them whole. A larger message still arrives:
/// the buffer grows to the frame the header announces, and is filled 4 KiB at a time. The write
/// buffer is left at its default, because it allocates nothing until a message is sent, and both
/// ends flush each message they send.
pub(crate) const READ_BUFFER_SIZE: usize = 4 * 1024;

/// The configuration of every WebSocket either end opens: the receive limit, the per-frame cap
/// moved with it, and the [`READ_BUFFER_SIZE`]. Left at its default, the per-frame cap would refuse
/// messages *below* the configured limit, which is the limit's business, not the framing's. One
/// function, so the Server's endpoint and the Client's socket cannot drift apart (DEV-six-crates-tokio-axum).
pub(crate) fn websocket_config(limit: usize) -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(READ_BUFFER_SIZE)
        .max_message_size(Some(limit))
        .max_frame_size(Some(limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WebSocket holds a read buffer sized to an Agent's messages, not `tungstenite`'s 128 KiB,
    /// while the receive limit and the per-frame cap stay the configured limit, and the write side
    /// stays at its defaults. The Server's endpoint and the Client's socket build their
    /// configuration nowhere else.
    /// Verifies: SEC-tls13-two-planes-bounded-input
    #[test]
    fn every_websocket_reads_into_a_small_buffer_under_the_configured_limit() {
        let default = WebSocketConfig::default();
        for limit in [4096, crate::frame::DEFAULT_MAX_MESSAGE_SIZE] {
            let config = websocket_config(limit);
            assert_eq!(config.read_buffer_size, 4 * 1024);
            assert!(config.read_buffer_size < default.read_buffer_size);
            assert_eq!(config.max_message_size, Some(limit));
            assert_eq!(config.max_frame_size, Some(limit));
            assert_eq!(config.write_buffer_size, default.write_buffer_size);
            assert_eq!(config.max_write_buffer_size, default.max_write_buffer_size);
            assert_eq!(
                config.accept_unmasked_frames,
                default.accept_unmasked_frames
            );
        }
        for (path, source) in [
            ("server.rs", include_str!("server.rs")),
            ("client/ws.rs", include_str!("client/ws.rs")),
        ] {
            assert!(
                source.contains("crate::websocket::websocket_config("),
                "{path} does not build its WebSocket configuration from websocket::websocket_config"
            );
            assert!(
                !source.contains("WebSocketConfig"),
                "{path} builds a WebSocket configuration of its own"
            );
        }
    }
}
