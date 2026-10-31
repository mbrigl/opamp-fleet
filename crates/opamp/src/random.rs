//! The randomness both ends draw their spread from: the reconnect jitter of the Client, and the
//! `Retry-After` and closing instants of a Server that sheds load (ADR-0012).

/// A uniform draw over `[0, bound)`, from the system's randomness — `ring` is already linked for
/// package signature verification, so this needs no new dependency for a handful of jitter bits.
///
/// A randomness source that will not answer is not worth failing over: the fallback is `0`, which
/// still gives a valid delay, just without the spread.
pub(crate) fn below(bound: u64) -> u64 {
    use ring::rand::SecureRandom as _;
    if bound == 0 {
        return 0;
    }
    let mut bytes = [0u8; 8];
    match ring::rand::SystemRandom::new().fill(&mut bytes) {
        Ok(()) => u64::from_le_bytes(bytes) % bound,
        Err(_) => 0,
    }
}
