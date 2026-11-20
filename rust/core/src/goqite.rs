//! A hand-written port of the parts of `maragu.dev/goqite` Postie uses (ADR-004): a durable SQLite
//! queue on the same `goqite` table and the same SQL, so a database written by the Go app is read
//! unchanged. Semantics: messages are received in `priority desc, created` order; a received message is
//! invisible until its timeout passes (default 5 s, extendable); a message can be received at most
//! `max_receive` times (default 3) and is then never delivered again; timestamps are text in
//! `%Y-%m-%dT%H:%M:%S.mmmZ` form and compared as strings, so the format must match exactly.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{OptionalExtension, params};

use crate::db::Db;

/// A received message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct Queue {
    db: Db,
    name: String,
    max_receive: i64,
    timeout: Duration,
}

/// goqite's `rfc3339Milli` in UTC.
pub(crate) fn fmt(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl Queue {
    /// `max_receive` 0 means 3; `timeout` zero means 5 seconds.
    pub fn new(db: Db, name: &str, max_receive: i64, timeout: Duration) -> Self {
        assert!(!name.is_empty(), "name cannot be empty");
        Self {
            db,
            name: name.to_string(),
            max_receive: if max_receive == 0 { 3 } else { max_receive },
            timeout: if timeout.is_zero() {
                Duration::seconds(5)
            } else {
                timeout
            },
        }
    }

    /// Enqueues `body` (visible after `delay`) and returns its id (`m_` + 32 hex characters).
    pub fn send(&self, body: &[u8], delay: Duration, priority: i64) -> rusqlite::Result<String> {
        let timeout = fmt(Utc::now() + delay.max(Duration::zero()));
        self.db.lock().query_row("insert into goqite (queue, body, timeout, priority) values (?1, ?2, ?3, ?4) returning id", params![self.name, body, timeout, priority], |r| r.get(0))
    }

    /// The best visible message, or `None`. Atomic: one UPDATE ... RETURNING.
    pub fn receive(&self) -> rusqlite::Result<Option<Message>> {
        let now = Utc::now();
        self.db
            .lock()
            .query_row(
                "update goqite
                 set timeout = ?1, received = received + 1
                 where id = (
                     select id from goqite
                     where queue = ?2 and ?3 >= timeout and received < ?4
                     order by priority desc, created
                     limit 1
                 )
                 returning id, body",
                params![
                    fmt(now + self.timeout),
                    self.name,
                    fmt(now),
                    self.max_receive
                ],
                |r| {
                    Ok(Message {
                        id: r.get(0)?,
                        body: r.get(1)?,
                    })
                },
            )
            .optional()
    }

    /// Pushes a received message's visibility timeout `delay` into the future.
    pub fn extend(&self, id: &str, delay: Duration) -> rusqlite::Result<()> {
        let timeout = fmt(Utc::now() + delay.max(Duration::zero()));
        self.db
            .lock()
            .execute(
                "update goqite set timeout = ?1 where queue = ?2 and id = ?3",
                params![timeout, self.name, id],
            )
            .map(|_| ())
    }

    pub fn delete(&self, id: &str) -> rusqlite::Result<()> {
        self.db
            .lock()
            .execute(
                "delete from goqite where queue = ?1 and id = ?2",
                params![self.name, id],
            )
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::MigrationRunner;

    fn queue() -> (Db, Queue) {
        let db = Db::in_memory().unwrap();
        MigrationRunner::new(db.clone()).migrate_up().unwrap();
        (db.clone(), Queue::new(db, "file_jobs", 0, Duration::zero()))
    }

    #[test]
    fn ids_have_the_goqite_shape() {
        let (_, q) = queue();
        let id = q.send(b"x", Duration::zero(), 0).unwrap();
        assert!(
            id.starts_with("m_")
                && id.len() == 34
                && id[2..].bytes().all(|b| b.is_ascii_hexdigit()),
            "{id}"
        );
    }

    #[test]
    fn receives_by_priority_then_creation_order() {
        let (db, q) = queue();
        let a = q.send(b"low-old", Duration::zero(), 0).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = q.send(b"low-new", Duration::zero(), 0).unwrap();
        let c = q.send(b"high", Duration::zero(), 5).unwrap();
        let order: Vec<String> = (0..3).map(|_| q.receive().unwrap().unwrap().id).collect();
        assert_eq!(order, [c, a, b]);
        assert_eq!(
            q.receive().unwrap(),
            None,
            "everything is invisible until the timeout"
        );
        let n: i64 = db
            .lock()
            .query_row("select count(*) from goqite", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn a_message_is_received_at_most_three_times() {
        let (db, q) = queue();
        let id = q.send(b"x", Duration::zero(), 0).unwrap();
        for n in 1..=3 {
            assert_eq!(q.receive().unwrap().unwrap().id, id, "receive {n}");
            // make it visible again
            db.lock()
                .execute("update goqite set timeout = '2000-01-01T00:00:00.000Z'", [])
                .unwrap();
        }
        assert_eq!(
            q.receive().unwrap(),
            None,
            "a fourth delivery never happens"
        );
    }

    #[test]
    fn delay_extend_and_delete() {
        let (db, q) = queue();
        let id = q.send(b"later", Duration::hours(1), 0).unwrap();
        assert_eq!(q.receive().unwrap(), None, "not visible before its delay");
        db.lock()
            .execute("update goqite set timeout = '2000-01-01T00:00:00.000Z'", [])
            .unwrap();
        assert_eq!(q.receive().unwrap().unwrap().body, b"later");
        q.extend(&id, Duration::hours(2)).unwrap();
        let t: String = db
            .lock()
            .query_row("select timeout from goqite", [], |r| r.get(0))
            .unwrap();
        assert!(t > fmt(Utc::now() + Duration::minutes(90)), "{t}");
        q.delete(&id).unwrap();
        assert_eq!(
            db.lock()
                .query_row("select count(*) from goqite", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        q.delete("m_missing").unwrap();
    }

    #[test]
    fn queues_are_isolated_by_name_and_timestamps_use_milliseconds() {
        let (db, q) = queue();
        let other = Queue::new(db.clone(), "other", 0, Duration::zero());
        other.send(b"o", Duration::zero(), 0).unwrap();
        assert_eq!(q.receive().unwrap(), None);
        q.send(b"m", Duration::zero(), 0).unwrap();
        let t: String = db
            .lock()
            .query_row(
                "select timeout from goqite where queue='file_jobs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            t.len() == 24 && t.ends_with('Z') && t.as_bytes()[19] == b'.',
            "{t}"
        );
    }

    /// A row written by Go (millisecond timestamps, default id) is received unchanged.
    #[test]
    fn go_written_rows_are_received() {
        let (db, q) = queue();
        db.lock().execute("insert into goqite (queue, body, timeout, priority) values ('file_jobs', x'7b7d', '2020-01-01T00:00:00.000Z', 2)", []).unwrap();
        let m = q.receive().unwrap().unwrap();
        assert_eq!(
            (m.body.as_slice(), m.id.starts_with("m_")),
            (b"{}".as_slice(), true)
        );
    }
}
