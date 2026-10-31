//! Ports for a Server binary a test starts, which binds them itself.

/// A loopback port for the Server under test to bind itself, nobody's at the moment.
///
/// A port handed out by binding port 0 and released goes back to the range the system assigns
/// every other bind to port 0 and every outgoing connection from, so a parallel test may hold it
/// by the time the Server binds it. These come from below that range on Linux, macOS and Windows,
/// which only an explicit bind takes, and each is handed out once per test binary.
pub fn spare_port() -> u16 {
    use std::sync::atomic::{AtomicU32, Ordering};
    const FIRST: u16 = 20_000;
    const SPAN: u16 = 12_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    // Test binaries apart start at different offsets, should two ever run at once.
    let start = std::process::id() % u32::from(SPAN);
    for _ in 0..SPAN {
        let offset = (start + NEXT.fetch_add(1, Ordering::Relaxed)) % u32::from(SPAN);
        let port = FIRST + u16::try_from(offset).expect("below SPAN");
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    panic!("no spare loopback port below the ephemeral range");
}
