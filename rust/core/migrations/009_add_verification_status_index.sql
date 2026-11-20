-- +goose Up
-- ListPendingVerificationItemIDs filters completed_items by
-- verification_status on every reconcile pass (startup and periodic); without
-- an index that is a full table scan on large queues.

CREATE INDEX IF NOT EXISTS idx_completed_items_verification_status
    ON completed_items (verification_status);

-- +goose Down
DROP INDEX IF EXISTS idx_completed_items_verification_status;
