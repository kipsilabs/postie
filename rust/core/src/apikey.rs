//! Port of `internal/apikey`: the persistent HTTP API key in the single-row `api_keys` table.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use rusqlite::{OptionalExtension, params};

use crate::db::Db;

/// Errors carry the Go message prefixes.
#[derive(Debug, thiserror::Error)]
pub enum ApiKeyError {
    #[error("read api key: {0}")]
    Read(rusqlite::Error),
    #[error("write api key: {0}")]
    Write(rusqlite::Error),
    #[error("api key cannot be empty")]
    Empty,
}

/// Reads and writes the single API key row.
pub trait Store {
    /// The stored key, or `None` when no key has been persisted yet.
    fn get(&self) -> Result<Option<String>, ApiKeyError>;
    /// Upserts the key into the single-row `api_keys` table.
    fn set(&self, key: &str) -> Result<(), ApiKeyError>;
}

/// SQLite-backed [`Store`].
pub struct SqlStore {
    db: Db,
}

impl SqlStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl Store for SqlStore {
    fn get(&self) -> Result<Option<String>, ApiKeyError> {
        self.db
            .lock()
            .query_row("SELECT key FROM api_keys WHERE id = 1", [], |r| r.get(0))
            .optional()
            .map_err(ApiKeyError::Read)
    }

    fn set(&self, key: &str) -> Result<(), ApiKeyError> {
        if key.is_empty() {
            return Err(ApiKeyError::Empty);
        }
        self.db
            .lock()
            .execute(
                "INSERT INTO api_keys (id, key) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET key = excluded.key,
                                               created_at = strftime('%Y-%m-%dT%H:%M:%fZ')",
                params![key],
            )
            .map(|_| ())
            .map_err(ApiKeyError::Write)
    }
}

/// A fresh URL-safe random key (32 bytes, 43 characters, no padding).
pub fn generate() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// The existing key, or a newly generated and persisted one. Idempotent.
pub fn ensure_key(store: &impl Store) -> Result<String, ApiKeyError> {
    if let Some(existing) = store.get()?.filter(|k| !k.is_empty()) {
        return Ok(existing);
    }
    let key = generate();
    store.set(&key)?;
    Ok(key)
}

/// Forces a new key, persists it and returns it.
pub fn regenerate(store: &impl Store) -> Result<String, ApiKeyError> {
    let key = generate();
    store.set(&key)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DDL from `internal/database/migrations/006_add_api_keys.sql`.
    const DDL: &str = "create table if not exists api_keys (
        id integer primary key check (id = 1),
        key text not null,
        created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ')));";

    fn store() -> SqlStore {
        let db = Db::in_memory().unwrap();
        db.lock().execute_batch(DDL).unwrap();
        SqlStore::new(db)
    }

    #[test]
    fn generate_is_unique_43_char_url_safe() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let k = generate();
            assert_eq!(k.len(), 43);
            assert!(
                k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            );
            assert!(seen.insert(k));
        }
    }

    #[test]
    fn ensure_key_generates_and_persists() {
        let s = store();
        assert_eq!(s.get().unwrap(), None);
        let first = ensure_key(&s).unwrap();
        assert_eq!(ensure_key(&s).unwrap(), first);
        assert_eq!(s.get().unwrap(), Some(first));
    }

    #[test]
    fn regenerate_replaces_key_in_single_row() {
        let s = store();
        let first = ensure_key(&s).unwrap();
        let second = regenerate(&s).unwrap();
        assert_ne!(first, second);
        assert_eq!(s.get().unwrap(), Some(second));
        let rows: i64 =
            s.db.lock()
                .query_row("select count(*) from api_keys", [], |r| r.get(0))
                .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn empty_key_rejected() {
        let s = store();
        assert!(matches!(s.set(""), Err(ApiKeyError::Empty)));
        assert_eq!(s.get().unwrap(), None);
    }

    #[test]
    fn empty_stored_key_is_regenerated_like_go() {
        // Go treats an empty stored key as "none" (existing != "").
        let s = store();
        s.db.lock()
            .execute("insert into api_keys (id, key) values (1, '')", [])
            .unwrap();
        let k = ensure_key(&s).unwrap();
        assert_eq!(k.len(), 43);
    }
}
