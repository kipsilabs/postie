-- +goose Up
-- Add script execution tracking columns to completed_items table

ALTER TABLE completed_items ADD COLUMN script_status TEXT DEFAULT NULL;
ALTER TABLE completed_items ADD COLUMN script_retry_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE completed_items ADD COLUMN script_last_error TEXT DEFAULT NULL;
ALTER TABLE completed_items ADD COLUMN script_next_retry_at TEXT DEFAULT NULL;

-- Create index for efficient script retry polling
CREATE INDEX IF NOT EXISTS completed_items_script_retry_idx
ON completed_items (script_status, script_next_retry_at)
WHERE script_status = 'pending_retry';

-- +goose Down
-- Remove script execution tracking

DROP INDEX IF EXISTS completed_items_script_retry_idx;
ALTER TABLE completed_items DROP COLUMN script_next_retry_at;
ALTER TABLE completed_items DROP COLUMN script_last_error;
ALTER TABLE completed_items DROP COLUMN script_retry_count;
ALTER TABLE completed_items DROP COLUMN script_status;
