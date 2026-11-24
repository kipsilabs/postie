//! Shared SQLite handle. One connection behind a mutex; callers run blocking work via
//! `tokio::task::spawn_blocking`. Port of the `*sql.DB` the Go units receive.

use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

/// Cloneable handle to the application database.
#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub fn new(conn: Connection) -> Self {
        Self(Arc::new(Mutex::new(conn)))
    }

    /// Opens an in-memory database (tests).
    pub fn in_memory() -> rusqlite::Result<Self> {
        Ok(Self::new(Connection::open_in_memory()?))
    }

    /// Locks the connection. A poisoned lock is recovered: a panic in another holder must not
    /// take the whole database down, and SQLite's own state is still consistent.
    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}
