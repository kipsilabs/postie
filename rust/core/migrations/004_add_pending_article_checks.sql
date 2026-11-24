-- +goose Up
-- Add pending article checks table for deferred post-check verification

CREATE TABLE IF NOT EXISTS pending_article_checks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    completed_item_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    groups TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    retry_count INTEGER NOT NULL DEFAULT 0,
    next_retry_at TEXT NOT NULL,
    first_failure_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')),
    last_checked_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')),
    FOREIGN KEY (completed_item_id) REFERENCES completed_items(id)
);

CREATE INDEX idx_pending_checks_status_retry ON pending_article_checks(status, next_retry_at);
CREATE INDEX idx_pending_checks_completed_item ON pending_article_checks(completed_item_id);

-- Add verification status to completed items
ALTER TABLE completed_items ADD COLUMN verification_status TEXT NOT NULL DEFAULT 'verified';

-- +goose Down
DROP TABLE IF EXISTS pending_article_checks;
