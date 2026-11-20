-- +goose Up
-- Add first failure timestamp for time-based retry duration tracking

ALTER TABLE completed_items ADD COLUMN script_first_failure_at TEXT DEFAULT NULL;

-- +goose Down
-- Remove first failure timestamp

ALTER TABLE completed_items DROP COLUMN script_first_failure_at;
