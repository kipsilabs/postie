//! Network timeouts for stuck connections. The values are fixed in production; integration tests
//! shorten them process-wide so a server that hangs is exercised in seconds instead of minutes.

use std::sync::RwLock;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// How long one article may take to be accepted before the post fails.
    pub post: Duration,
    /// How long a connection may go without progress (no bytes written, no reply) before it is dropped.
    pub stall: Duration,
    /// How long connecting, the greeting and authentication may take.
    pub connect: Duration,
}

pub const DEFAULT: Timeouts = Timeouts {
    post: Duration::from_secs(120),
    stall: Duration::from_secs(120),
    connect: Duration::from_secs(30),
};

static OVERRIDE: RwLock<Option<Timeouts>> = RwLock::new(None);

/// The timeouts in effect.
pub fn current() -> Timeouts {
    OVERRIDE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .unwrap_or(DEFAULT)
}

/// Replaces the timeouts for the whole process; for tests only. Pools built afterwards use them.
pub fn set_for_tests(t: Timeouts) {
    *OVERRIDE.write().unwrap_or_else(|e| e.into_inner()) = Some(t);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_production_values() {
        assert_eq!(DEFAULT.post, Duration::from_secs(120));
        assert_eq!(DEFAULT.stall, Duration::from_secs(120));
        assert_eq!(DEFAULT.connect, Duration::from_secs(30));
    }
}
