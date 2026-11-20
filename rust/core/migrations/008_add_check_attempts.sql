-- +goose Up
-- check_attempts counts failed attempts to run a file's first verification
-- check (e.g. the manifest could not be opened). It lets the verification
-- service back off and eventually terminalize the file instead of retrying a
-- broken file every poll cycle forever (issue #168: stuck pending verification).

ALTER TABLE transfer_files ADD COLUMN check_attempts INTEGER NOT NULL DEFAULT 0;

-- +goose Down
ALTER TABLE transfer_files DROP COLUMN check_attempts;
