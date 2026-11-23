//! Port of `internal/wshub`: fans backend events out to browser clients in web mode.
//!
//! This is the transport-agnostic hub. The WebSocket upgrade, ping/pong and write deadlines live
//! with the web server (axum) and use the constants below. A client that stops draining its
//! queue is dropped so one stalled tab cannot hold the hub (same rule as the Go hub).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const PING_INTERVAL: Duration = Duration::from_secs(30);
pub const PONG_WAIT: Duration = Duration::from_secs(60);
pub const WRITE_WAIT: Duration = Duration::from_secs(10);
pub const SEND_BUFFER_SIZE: usize = 256;

/// Wire envelope: `{"type": ..., "data": ...}`.
#[derive(Serialize)]
struct Message<'a, T: Serialize> {
    r#type: &'a str,
    data: T,
}

struct Client {
    session: String,
    tx: mpsc::Sender<Arc<str>>,
    closed: CancellationToken,
}

/// A registered client: read `rx` and write each message to the socket until `closed` fires.
pub struct Subscription {
    pub id: u64,
    pub rx: mpsc::Receiver<Arc<str>>,
    /// Cancelled when the hub drops the client (too slow) or its session is disconnected.
    pub closed: CancellationToken,
}

#[derive(Default)]
struct State {
    next_id: u64,
    clients: HashMap<u64, Client>,
}

/// Cloneable hub handle.
#[derive(Clone, Default)]
pub struct Hub {
    state: Arc<Mutex<State>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Registers a client for `session` (empty when sessions are not used).
    pub fn register(&self, session: &str) -> Subscription {
        let (tx, rx) = mpsc::channel(SEND_BUFFER_SIZE);
        let closed = CancellationToken::new();
        let mut s = self.lock();
        s.next_id += 1;
        let id = s.next_id;
        s.clients.insert(
            id,
            Client {
                session: session.to_string(),
                tx,
                closed: closed.clone(),
            },
        );
        Subscription { id, rx, closed }
    }

    /// Removes a client (call when its socket ends).
    pub fn unregister(&self, id: u64) {
        self.lock().clients.remove(&id);
    }

    pub fn client_count(&self) -> usize {
        self.lock().clients.len()
    }

    /// Sends an event to every connected client. Serialization failures are logged and the event
    /// dropped; clients whose queue is full are dropped.
    pub fn emit_event<T: Serialize>(&self, event_type: &str, data: T) {
        let payload: Arc<str> = match serde_json::to_string(&Message {
            r#type: event_type,
            data,
        }) {
            Ok(p) => p.into(),
            Err(e) => {
                crate::logx::warn(&format!("Error marshaling WebSocket message: {e}"), &[]);
                return;
            }
        };
        let mut s = self.lock();
        s.clients
            .retain(|id, c| match c.tx.try_send(payload.clone()) {
                Ok(()) => true,
                Err(_) => {
                    crate::logx::warn(&format!("WebSocket client too slow, dropping id={id}"), &[]);
                    c.closed.cancel();
                    false
                }
            });
    }

    /// Immediately stops every connection that belongs to `session` (logout).
    pub fn disconnect_session(&self, session: &str) {
        for c in self
            .lock()
            .clients
            .values()
            .filter(|c| c.session == session)
        {
            c.closed.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn events_reach_every_client_as_type_data_json() {
        let hub = Hub::new();
        let mut a = hub.register("");
        let mut b = hub.register("");
        hub.emit_event("progress", serde_json::json!({"n": 1}));
        for s in [&mut a, &mut b] {
            let m = s.rx.recv().await.unwrap();
            assert_eq!(&*m, r#"{"type":"progress","data":{"n":1}}"#);
        }
    }

    #[tokio::test]
    async fn events_after_reconnect_are_delivered() {
        let hub = Hub::new();
        let first = hub.register("");
        hub.unregister(first.id);
        drop(first);
        let mut second = hub.register("");
        hub.emit_event("e", 1);
        assert_eq!(
            &*second.rx.recv().await.unwrap(),
            r#"{"type":"e","data":1}"#
        );
        assert_eq!(hub.client_count(), 1);
    }

    #[tokio::test]
    async fn slow_client_is_dropped_without_blocking_others() {
        let hub = Hub::new();
        let slow = hub.register("");
        let mut fast = hub.register("");
        for i in 0..=SEND_BUFFER_SIZE {
            hub.emit_event("e", i);
            // the fast client keeps draining
            let _ = fast.rx.recv().await;
        }
        assert!(slow.closed.is_cancelled(), "slow client should be dropped");
        assert!(!fast.closed.is_cancelled());
        assert_eq!(hub.client_count(), 1);
    }

    #[tokio::test]
    async fn disconnect_session_closes_only_that_session() {
        let hub = Hub::new();
        let a = hub.register("s1");
        let b = hub.register("s2");
        hub.disconnect_session("s1");
        assert!(a.closed.is_cancelled());
        assert!(!b.closed.is_cancelled());
    }
}
