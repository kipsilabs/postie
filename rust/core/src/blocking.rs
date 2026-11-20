//! Runs short blocking work (SQLite behind the global mutex, `fsync`, file metadata) from async
//! code without parking a runtime worker thread: on a multi-thread runtime the worker is handed
//! off (`block_in_place`) so other tasks keep running; on a current-thread runtime (tests) the
//! closure just runs inline.

use tokio::runtime::{Handle, RuntimeFlavor};

pub fn in_place<R>(f: impl FnOnce() -> R) -> R {
    match Handle::try_current() {
        Ok(h) if h.runtime_flavor() == RuntimeFlavor::MultiThread => tokio::task::block_in_place(f),
        _ => f(),
    }
}

#[cfg(test)]
mod tests {
    use super::in_place;

    #[test]
    fn runs_without_a_runtime() {
        assert_eq!(in_place(|| 7), 7);
    }

    #[tokio::test]
    async fn runs_on_a_current_thread_runtime() {
        assert_eq!(in_place(|| 8), 8);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runs_on_a_multi_thread_runtime() {
        assert_eq!(in_place(|| 9), 9);
    }
}
