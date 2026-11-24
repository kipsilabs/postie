//! Port of `internal/pausable`: a pause gate that long-running work checks between steps.
//!
//! Go wraps `context.Context` and finds the gate through the context chain (`FromContext`).
//! In Rust the gate is a cloneable handle passed explicitly; cancellation is a
//! [`CancellationToken`]. `Resume` wakes every waiter (a watch channel broadcasts).

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Returned by [`PauseGate::check_pause`] when the token is cancelled while paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("context canceled")]
pub struct Canceled;

/// Cloneable pause/resume gate tied to a cancellation token.
#[derive(Clone)]
pub struct PauseGate {
    paused: watch::Sender<bool>,
    cancel: CancellationToken,
}

impl PauseGate {
    /// A new, unpaused gate that observes `cancel`.
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            paused: watch::Sender::new(false),
            cancel,
        }
    }

    /// Pauses; idempotent.
    pub fn pause(&self) {
        self.paused.send_replace(true);
    }

    /// Resumes and wakes every task blocked in [`check_pause`](Self::check_pause); idempotent.
    pub fn resume(&self) {
        self.paused.send_replace(false);
    }

    pub fn is_paused(&self) -> bool {
        *self.paused.borrow()
    }

    /// Returns at once when not paused; otherwise waits until resumed, or fails with
    /// [`Canceled`] when the token is cancelled first.
    pub async fn check_pause(&self) -> Result<(), Canceled> {
        let mut rx = self.paused.subscribe();
        loop {
            if !*rx.borrow_and_update() {
                return Ok(());
            }
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(Canceled),
                changed = rx.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn pause_and_resume_toggle_state() {
        let g = PauseGate::new(CancellationToken::new());
        assert!(!g.is_paused());
        g.pause();
        g.pause();
        assert!(g.is_paused());
        g.resume();
        g.resume();
        assert!(!g.is_paused());
    }

    #[tokio::test]
    async fn unpaused_check_returns_immediately() {
        let g = PauseGate::new(CancellationToken::new());
        assert_eq!(g.check_pause().await, Ok(()));
    }

    #[tokio::test]
    async fn check_blocks_until_resume() {
        let g = PauseGate::new(CancellationToken::new());
        g.pause();
        let g2 = g.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            g2.resume();
        });
        let start = Instant::now();
        assert_eq!(g.check_pause().await, Ok(()));
        assert!(start.elapsed() >= Duration::from_millis(40));
    }

    #[tokio::test]
    async fn cancel_while_paused_returns_canceled() {
        let token = CancellationToken::new();
        let g = PauseGate::new(token.clone());
        g.pause();
        token.cancel();
        assert_eq!(g.check_pause().await, Err(Canceled));
    }

    #[tokio::test]
    async fn resume_wakes_every_waiter() {
        let g = PauseGate::new(CancellationToken::new());
        g.pause();
        let waiters: Vec<_> = (0..16)
            .map(|_| {
                let g = g.clone();
                tokio::spawn(async move { g.check_pause().await })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(20)).await;
        g.resume();
        for w in waiters {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), w)
                    .await
                    .unwrap()
                    .unwrap(),
                Ok(())
            );
        }
    }
}
