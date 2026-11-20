-- +goose Up
-- Durable transfer tracking for the process-wide upload architecture (issue 184).
-- transfer_files records one row per source/PAR2 file in a transfer, pointing at
-- its immutable manifest. Individual successful articles are NOT stored (a large
-- upload can contain millions); only failed articles are tracked, in
-- verification_failures.

CREATE TABLE IF NOT EXISTS transfer_files (
    transfer_id        TEXT NOT NULL,
    file_id            TEXT NOT NULL,
    completed_item_id  TEXT,
    manifest_path      TEXT NOT NULL,
    manifest_version   INTEGER NOT NULL DEFAULT 1,
    source_path        TEXT NOT NULL,
    file_role          TEXT NOT NULL DEFAULT 'original',
    article_count      INTEGER NOT NULL DEFAULT 0,
    upload_state       TEXT NOT NULL DEFAULT 'planned',
    verification_state TEXT NOT NULL DEFAULT 'planned',
    posted_at          TEXT,
    next_check_at      TEXT,
    cleanup_policy     TEXT NOT NULL DEFAULT '',
    last_error         TEXT NOT NULL DEFAULT '',
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (transfer_id, file_id)
);

CREATE INDEX IF NOT EXISTS idx_transfer_files_completed_item ON transfer_files(completed_item_id);
-- Drives the verification service's "earliest due check" scheduling.
CREATE INDEX IF NOT EXISTS idx_transfer_files_next_check ON transfer_files(verification_state, next_check_at);

-- verification_failures stores ONLY articles that failed a STAT check, so the
-- durable verification service can re-post or re-check them. Leases let multiple
-- workers/instances claim disjoint batches and recover after a crash.
CREATE TABLE IF NOT EXISTS verification_failures (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    transfer_id        TEXT NOT NULL,
    file_id            TEXT NOT NULL,
    article_index      INTEGER NOT NULL DEFAULT -1,
    message_id         TEXT NOT NULL,
    groups             TEXT NOT NULL DEFAULT '',
    repost_count       INTEGER NOT NULL DEFAULT 0,
    deferred_count     INTEGER NOT NULL DEFAULT 0,
    state              TEXT NOT NULL DEFAULT 'pending',
    next_attempt_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    lease_owner        TEXT NOT NULL DEFAULT '',
    lease_expires_at   TEXT,
    last_error         TEXT NOT NULL DEFAULT '',
    last_checked_at    TEXT,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (transfer_id, file_id, message_id)
);

CREATE INDEX IF NOT EXISTS idx_verification_failures_due ON verification_failures(state, next_attempt_at);
CREATE INDEX IF NOT EXISTS idx_verification_failures_lease ON verification_failures(lease_owner, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_verification_failures_transfer ON verification_failures(transfer_id, file_id);

-- +goose Down
DROP TABLE IF EXISTS verification_failures;
DROP TABLE IF EXISTS transfer_files;
