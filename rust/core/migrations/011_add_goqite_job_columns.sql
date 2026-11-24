-- +goose Up
-- The dashboard sorts and sums the pending queue by fields that live inside the JSON job body.
-- Virtual generated columns expose them without changing how goqite reads or writes rows, and the
-- `json_valid` guards keep goqite's arbitrary-bytes bodies insertable. The indexes turn those per-poll full scans (json_extract on every row, under the global DB mutex)
-- into index reads. Requires SQLite 3.31+.

ALTER TABLE goqite ADD COLUMN job_path TEXT GENERATED ALWAYS AS (CASE WHEN json_valid(body) THEN json_extract(body, '$.path') END) VIRTUAL;
ALTER TABLE goqite ADD COLUMN job_size INTEGER GENERATED ALWAYS AS (CASE WHEN json_valid(body) THEN CAST(json_extract(body, '$.size') AS INTEGER) END) VIRTUAL;

CREATE INDEX IF NOT EXISTS goqite_queue_job_path_idx ON goqite (queue, job_path);
CREATE INDEX IF NOT EXISTS goqite_queue_job_size_idx ON goqite (queue, job_size);

-- +goose Down
DROP INDEX IF EXISTS goqite_queue_job_size_idx;
DROP INDEX IF EXISTS goqite_queue_job_path_idx;
ALTER TABLE goqite DROP COLUMN job_size;
ALTER TABLE goqite DROP COLUMN job_path;
