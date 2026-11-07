//! Helpers the unit tests of this crate share: what they log, read back from one global
//! subscriber of the test process.
//!
//! A subscriber set for one test with `tracing::subscriber::with_default` misses lines another test
//! reached first on a thread with none: `tracing` caches whether a line is wanted when it is first
//! reached. The global one is wanted everywhere, so every line reaches it, whichever thread logs
//! it; a test tells its own lines apart by what they name.

use std::sync::{Arc, Mutex, OnceLock};

/// Every line logged since the first call, on any thread.
pub(crate) fn logged() -> &'static Mutex<Vec<u8>> {
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let log = Arc::new(Mutex::new(Vec::new()));
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || LogBuffer(writer.clone()))
            .with_ansi(false)
            .finish();
        // Setting it asks every line reached before again whether it is wanted.
        let _ = tracing::subscriber::set_global_default(subscriber);
        log
    })
}

/// The lines logged so far that contain every one of `parts`.
pub(crate) fn lines_with(parts: &[&str]) -> Vec<String> {
    String::from_utf8_lossy(&logged().lock().expect("log"))
        .lines()
        .filter(|line| parts.iter().all(|part| line.contains(part)))
        .map(str::to_string)
        .collect()
}

/// A `tracing` writer into a shared buffer.
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
