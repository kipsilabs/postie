//! Port of `internal/database`: opening the SQLite database and running the goose-format
//! migrations. The `goose_db_version` table keeps goose's exact shape and semantics (one row per
//! applied version, version 0 seeded, a rollback deletes the row), so a database created by the Go
//! app opens unchanged here and the other way round.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::db::Db;

/// Migrations embedded from the Go source of truth while both servers coexist
/// (`rust/core/tests` checks this list against the directory).
pub const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "001_initial_schema.sql",
        include_str!("../../../internal/database/migrations/001_initial_schema.sql"),
    ),
    (
        2,
        "002_add_script_execution_tracking.sql",
        include_str!("../../../internal/database/migrations/002_add_script_execution_tracking.sql"),
    ),
    (
        3,
        "003_add_script_first_failure_at.sql",
        include_str!("../../../internal/database/migrations/003_add_script_first_failure_at.sql"),
    ),
    (
        4,
        "004_add_pending_article_checks.sql",
        include_str!("../../../internal/database/migrations/004_add_pending_article_checks.sql"),
    ),
    (
        5,
        "005_add_in_progress_items.sql",
        include_str!("../../../internal/database/migrations/005_add_in_progress_items.sql"),
    ),
    (
        6,
        "006_add_api_keys.sql",
        include_str!("../../../internal/database/migrations/006_add_api_keys.sql"),
    ),
    (
        7,
        "007_add_transfer_files.sql",
        include_str!("../../../internal/database/migrations/007_add_transfer_files.sql"),
    ),
    (
        8,
        "008_add_check_attempts.sql",
        include_str!("../../../internal/database/migrations/008_add_check_attempts.sql"),
    ),
    (
        9,
        "009_add_verification_status_index.sql",
        include_str!("../../../internal/database/migrations/009_add_verification_status_index.sql"),
    ),
    (
        10,
        "010_add_web_auth.sql",
        include_str!("../../../internal/database/migrations/010_add_web_auth.sql"),
    ),
];

#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("database type {0} is not yet implemented, please use 'sqlite'")]
    UnsupportedType(String),
    #[error("{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("failed to {0}: {1}")]
    Step(&'static str, rusqlite::Error),
}

/// Result of [`MigrationRunner::status`]; JSON matches the Go struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MigrationStatus {
    #[serde(rename = "currentVersion")]
    pub current_version: i64,
}

/// Opens the database like the Go app: WAL, 5 s busy timeout, foreign keys on.
pub fn open(database_type: &str, path: &str) -> Result<Db, DatabaseError> {
    if database_type != "sqlite" {
        return Err(DatabaseError::UnsupportedType(database_type.to_string()));
    }
    let path = if path.is_empty() { "postie.db" } else { path };
    let conn = Connection::open(Path::new(path))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(Db::new(conn))
}

/// Splits a goose file into its Up and Down sections.
fn sections(sql: &str) -> (&str, &str) {
    let up_at = sql.find("-- +goose Up");
    let down_at = sql.find("-- +goose Down");
    match (up_at, down_at) {
        (Some(u), Some(d)) if u < d => (
            &sql[u + "-- +goose Up".len()..d],
            &sql[d + "-- +goose Down".len()..],
        ),
        (Some(u), _) => (&sql[u + "-- +goose Up".len()..], ""),
        _ => ("", ""),
    }
}

pub struct MigrationRunner {
    db: Db,
}

impl MigrationRunner {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Creates the goose version table (seeded with version 0) when missing.
    fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT name FROM sqlite_master WHERE type='table' AND name='goose_db_version')",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            conn.execute_batch(
                "CREATE TABLE goose_db_version (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    version_id INTEGER NOT NULL,
                    is_applied INTEGER NOT NULL,
                    tstamp TIMESTAMP DEFAULT (datetime('now'))
                )",
            )?;
            conn.execute(
                "INSERT INTO goose_db_version (version_id, is_applied) VALUES (0, 1)",
                [],
            )?;
        }
        Ok(())
    }

    /// The latest applied version (goose's `GetDBVersion`).
    fn version(conn: &Connection) -> rusqlite::Result<i64> {
        Self::ensure_table(conn)?;
        let mut stmt =
            conn.prepare("SELECT version_id, is_applied FROM goose_db_version ORDER BY id DESC")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            if r.get::<_, i64>(1)? != 0 {
                return r.get(0);
            }
        }
        Ok(0)
    }

    fn up_to(&self, target: i64) -> Result<(), DatabaseError> {
        let mut c = self.db.lock();
        let current = Self::version(&c).map_err(|e| DatabaseError::Step("run migrations", e))?;
        for &(version, _, sql) in MIGRATIONS.iter().filter(|m| m.0 > current && m.0 <= target) {
            let tx = c
                .transaction()
                .map_err(|e| DatabaseError::Step("run migrations", e))?;
            tx.execute_batch(sections(sql).0)
                .map_err(|e| DatabaseError::Step("run migrations", e))?;
            tx.execute(
                "INSERT INTO goose_db_version (version_id, is_applied) VALUES (?1, 1)",
                params![version],
            )
            .map_err(|e| DatabaseError::Step("run migrations", e))?;
            tx.commit()
                .map_err(|e| DatabaseError::Step("run migrations", e))?;
        }
        Ok(())
    }

    /// Runs all pending migrations.
    pub fn migrate_up(&self) -> Result<(), DatabaseError> {
        self.up_to(i64::MAX)
    }

    /// Migrates up to (and including) `version`.
    pub fn migrate_to(&self, version: i64) -> Result<(), DatabaseError> {
        self.up_to(version).map_err(|e| match e {
            DatabaseError::Step(_, s) => DatabaseError::Step("migrate to version", s),
            other => other,
        })
    }

    /// Rolls back the latest applied migration (a no-op at version 0).
    pub fn migrate_down(&self) -> Result<(), DatabaseError> {
        self.down_one().map(|_| ())
    }

    fn down_one(&self) -> Result<bool, DatabaseError> {
        let mut c = self.db.lock();
        let current =
            Self::version(&c).map_err(|e| DatabaseError::Step("rollback migration", e))?;
        let Some(&(version, _, sql)) = MIGRATIONS.iter().find(|m| m.0 == current) else {
            return Ok(false);
        };
        let tx = c
            .transaction()
            .map_err(|e| DatabaseError::Step("rollback migration", e))?;
        tx.execute_batch(sections(sql).1)
            .map_err(|e| DatabaseError::Step("rollback migration", e))?;
        tx.execute(
            "DELETE FROM goose_db_version WHERE version_id=?1",
            params![version],
        )
        .map_err(|e| DatabaseError::Step("rollback migration", e))?;
        tx.commit()
            .map_err(|e| DatabaseError::Step("rollback migration", e))?;
        Ok(true)
    }

    /// Rolls back every migration (goose `Reset`).
    pub fn reset(&self) -> Result<(), DatabaseError> {
        while self.down_one()? {}
        Ok(())
    }

    pub fn status(&self) -> Result<MigrationStatus, DatabaseError> {
        let c = self.db.lock();
        Ok(MigrationStatus {
            current_version: Self::version(&c)
                .map_err(|e| DatabaseError::Step("get current version", e))?,
        })
    }

    /// True when the database has a `goqite` table but no goose version table.
    pub fn is_legacy_database(&self) -> Result<bool, DatabaseError> {
        let c = self.db.lock();
        let has = |name: &str| -> rusqlite::Result<bool> {
            c.query_row(
                "SELECT EXISTS(SELECT name FROM sqlite_master WHERE type='table' AND name=?1)",
                params![name],
                |r| r.get(0),
            )
        };
        let goqite = has("goqite").map_err(|e| DatabaseError::Step("check for goqite table", e))?;
        if !goqite {
            return Ok(false);
        }
        let goose = has("goose_db_version")
            .map_err(|e| DatabaseError::Step("check for goose version table", e))?;
        Ok(!goose)
    }

    /// Drops every table, then re-runs all migrations.
    pub fn recreate_database(&self) -> Result<(), DatabaseError> {
        {
            let mut c = self.db.lock();
            let tables: Vec<String> = {
                let mut stmt = c.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?;
                stmt.query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?
            };
            let tx = c
                .transaction()
                .map_err(|e| DatabaseError::Step("begin transaction", e))?;
            tx.execute_batch("PRAGMA foreign_keys = OFF")?;
            for t in tables {
                tx.execute_batch(&format!("DROP TABLE IF EXISTS {t}"))?;
            }
            tx.commit()
                .map_err(|e| DatabaseError::Step("commit table drops", e))?;
            c.execute_batch("PRAGMA foreign_keys = ON")?;
        }
        self.migrate_up()
    }

    /// Recreates a legacy (pre-goose) database; otherwise a normal `migrate_up`.
    pub fn ensure_migration_compatibility(&self) -> Result<(), DatabaseError> {
        if self.is_legacy_database()? {
            return self.recreate_database();
        }
        self.migrate_up()
    }

    /// True when the `completed_items`-style tables exist (helper for tests and diagnostics).
    pub fn table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        Ok(self
            .db
            .lock()
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                params![name],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> MigrationRunner {
        MigrationRunner::new(Db::in_memory().unwrap())
    }

    #[test]
    fn embedded_list_matches_the_migrations_directory() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../internal/database/migrations"
        );
        let mut on_disk: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".sql"))
            .collect();
        on_disk.sort();
        let listed: Vec<&str> = MIGRATIONS.iter().map(|m| m.1).collect();
        assert_eq!(on_disk, listed, "add new migrations to MIGRATIONS");
    }

    #[test]
    fn up_down_to_and_reset_follow_goose_semantics() {
        let r = runner();
        assert_eq!(r.status().unwrap().current_version, 0);
        r.migrate_to(7).unwrap();
        assert_eq!(r.status().unwrap().current_version, 7);
        assert!(r.table_exists("transfer_files").unwrap() && !r.table_exists("web_admin").unwrap());
        r.migrate_up().unwrap();
        r.migrate_up().unwrap(); // idempotent
        assert_eq!(r.status().unwrap().current_version, 10);
        r.migrate_down().unwrap();
        assert_eq!(r.status().unwrap().current_version, 9);
        assert!(!r.table_exists("web_admin").unwrap());
        r.reset().unwrap();
        assert_eq!(r.status().unwrap().current_version, 0);
        assert!(!r.table_exists("transfer_files").unwrap());
    }

    /// A database migrated by the Go app (goose) opens unchanged and is already at version 10.
    #[test]
    fn go_migrated_database_is_recognized() {
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/go-migrated.db");
        let tmp = std::env::temp_dir().join(format!(
            "go-migrated-{}-{}.db",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::copy(src, &tmp).unwrap();
        let db = open("sqlite", tmp.to_str().unwrap()).unwrap();
        let r = MigrationRunner::new(db.clone());
        assert_eq!(r.status().unwrap().current_version, 10);
        assert!(!r.is_legacy_database().unwrap());
        r.ensure_migration_compatibility().unwrap();
        let rows: i64 = db
            .lock()
            .query_row("select count(*) from goose_db_version", [], |x| x.get(0))
            .unwrap();
        assert_eq!(rows, 11, "no rows added to a current database");
        // rolling back one version then up again works against the Go-created history
        r.migrate_down().unwrap();
        r.migrate_up().unwrap();
        assert_eq!(r.status().unwrap().current_version, 10);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn rust_created_database_has_the_same_goose_rows() {
        let r = runner();
        r.migrate_up().unwrap();
        let rows: Vec<(i64, i64)> = {
            let c = r.db.lock();
            let mut s = c
                .prepare("select version_id,is_applied from goose_db_version order by id")
                .unwrap();
            s.query_map([], |x| Ok((x.get(0)?, x.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(rows, (0..=10).map(|v| (v, 1)).collect::<Vec<_>>());
    }

    #[test]
    fn legacy_database_is_detected_and_recreated() {
        let r = runner();
        r.db.lock()
            .execute_batch("create table goqite (id text); create table old_stuff (x int);")
            .unwrap();
        assert!(r.is_legacy_database().unwrap());
        r.ensure_migration_compatibility().unwrap();
        assert!(!r.table_exists("old_stuff").unwrap());
        assert_eq!(r.status().unwrap().current_version, 10);
        assert!(!r.is_legacy_database().unwrap());
    }

    #[test]
    fn unsupported_type_is_rejected() {
        assert!(
            matches!(open("postgres", "x"), Err(DatabaseError::UnsupportedType(t)) if t == "postgres")
        );
    }
}
