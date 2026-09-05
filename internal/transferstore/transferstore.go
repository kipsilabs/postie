// Package transferstore is the durable persistence layer for the process-wide
// upload architecture (issue 184). It manages the transfer_files table (one row
// per source/PAR2 file, pointing at an immutable manifest) and the
// verification_failures table (only articles that failed a STAT check, with
// database leases so verification survives crashes).
//
// Successful articles are deliberately never stored: a large upload can contain
// millions, and per-article rows would overwhelm SQLite.
package transferstore

import (
	"context"
	"database/sql"
	"fmt"
	"strings"
	"time"
)

// Upload / verification states for a transfer file.
const (
	StatePlanned            = "planned"
	StateUploading          = "uploading"
	StateUploaded           = "uploaded"
	StateVerifying          = "verifying"
	StateVerified           = "verified"
	StateVerificationFailed = "verification_failed"
)

// Cleanup policy markers stored on transfer_files.cleanup_policy. Empty means
// retain the source. They drive the post-verification cleanup.
const (
	CleanupDeleteOriginal = "delete_original"
)

// LegacyFileID is the synthetic file id used for verification_failures migrated
// from the pre-durable pending_article_checks table. These records have no
// manifest, so they are STAT-only (article_index = -1, never re-posted).
const LegacyFileID = "legacy"

// Verification-failure states. A re-posted article goes back to
// FailurePending with a propagation-delay recheck; there is no separate
// "reposted" state.
const (
	FailurePending  = "pending"
	FailureResolved = "resolved"
	FailureFailed   = "failed"
)

// TransferFile is one row of the transfer_files table.
type TransferFile struct {
	TransferID        string
	FileID            string
	CompletedItemID   string
	ManifestPath      string
	ManifestVersion   int
	SourcePath        string
	FileRole          string
	ArticleCount      int
	UploadState       string
	VerificationState string
	PostedAt          *time.Time
	NextCheckAt       *time.Time
	CleanupPolicy     string
	LastError         string
	// CheckAttempts counts failed attempts to start this file's first
	// verification check (e.g. unreadable manifest), so the service can back
	// off and eventually terminalize instead of retrying forever.
	CheckAttempts int
}

// VerificationFailure is one row of the verification_failures table.
type VerificationFailure struct {
	ID             int64
	TransferID     string
	FileID         string
	ArticleIndex   int
	MessageID      string
	Groups         []string
	RepostCount    int
	DeferredCount  int
	State          string
	NextAttemptAt  time.Time
	LeaseOwner     string
	LeaseExpiresAt *time.Time
	LastError      string
	LastCheckedAt  *time.Time
}

// Store provides durable access to transfer files and verification failures.
type Store struct {
	db *sql.DB
}

// New returns a Store backed by db. The transfer_files and verification_failures
// tables must already exist (migration 007).
func New(db *sql.DB) *Store {
	return &Store{db: db}
}

const tsLayout = time.RFC3339Nano

func fmtTime(t time.Time) string { return t.UTC().Format(tsLayout) }

func fmtTimePtr(t *time.Time) sql.NullString {
	if t == nil {
		return sql.NullString{}
	}
	return sql.NullString{String: fmtTime(*t), Valid: true}
}

// parseTime parses a timestamp written by Go (RFC3339Nano) or by SQLite's
// strftime default ("2006-01-02T15:04:05.000Z" / without fraction).
func parseTime(s string) (time.Time, error) {
	for _, layout := range []string{time.RFC3339Nano, "2006-01-02T15:04:05.999999999Z07:00", "2006-01-02T15:04:05Z07:00", "2006-01-02T15:04:05.999Z", "2006-01-02T15:04:05Z"} {
		if t, err := time.Parse(layout, s); err == nil {
			return t.UTC(), nil
		}
	}
	return time.Time{}, fmt.Errorf("unparseable timestamp %q", s)
}

func parseTimePtr(ns sql.NullString) (*time.Time, error) {
	if !ns.Valid || ns.String == "" {
		return nil, nil
	}
	t, err := parseTime(ns.String)
	if err != nil {
		return nil, err
	}
	return &t, nil
}

func encodeGroups(groups []string) string { return strings.Join(groups, ",") }

func decodeGroups(s string) []string {
	if s == "" {
		return nil
	}
	return strings.Split(s, ",")
}

// UpsertFile inserts or replaces a transfer_files row, refreshing updated_at.
func (s *Store) UpsertFile(ctx context.Context, f TransferFile) error {
	if f.ManifestVersion == 0 {
		f.ManifestVersion = 1
	}
	if f.UploadState == "" {
		f.UploadState = StatePlanned
	}
	if f.VerificationState == "" {
		f.VerificationState = StatePlanned
	}
	now := fmtTime(time.Now())
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO transfer_files (
			transfer_id, file_id, completed_item_id, manifest_path, manifest_version,
			source_path, file_role, article_count, upload_state, verification_state,
			posted_at, next_check_at, cleanup_policy, last_error, updated_at
		) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
		ON CONFLICT(transfer_id, file_id) DO UPDATE SET
			completed_item_id = excluded.completed_item_id,
			manifest_path     = excluded.manifest_path,
			manifest_version  = excluded.manifest_version,
			source_path       = excluded.source_path,
			file_role         = excluded.file_role,
			article_count     = excluded.article_count,
			upload_state      = excluded.upload_state,
			verification_state= excluded.verification_state,
			posted_at         = excluded.posted_at,
			next_check_at     = excluded.next_check_at,
			cleanup_policy    = excluded.cleanup_policy,
			last_error        = excluded.last_error,
			updated_at        = excluded.updated_at
	`,
		f.TransferID, f.FileID, nullStr(f.CompletedItemID), f.ManifestPath, f.ManifestVersion,
		f.SourcePath, f.FileRole, f.ArticleCount, f.UploadState, f.VerificationState,
		fmtTimePtr(f.PostedAt), fmtTimePtr(f.NextCheckAt), f.CleanupPolicy, f.LastError, now,
	)
	if err != nil {
		return fmt.Errorf("upsert transfer file: %w", err)
	}
	return nil
}

func nullStr(s string) sql.NullString {
	if s == "" {
		return sql.NullString{}
	}
	return sql.NullString{String: s, Valid: true}
}

const transferFileCols = `transfer_id, file_id, completed_item_id, manifest_path, manifest_version,
	source_path, file_role, article_count, upload_state, verification_state,
	posted_at, next_check_at, cleanup_policy, last_error, check_attempts`

func scanTransferFile(sc interface{ Scan(...any) error }) (TransferFile, error) {
	var (
		f         TransferFile
		completed sql.NullString
		postedAt  sql.NullString
		nextCheck sql.NullString
	)
	if err := sc.Scan(
		&f.TransferID, &f.FileID, &completed, &f.ManifestPath, &f.ManifestVersion,
		&f.SourcePath, &f.FileRole, &f.ArticleCount, &f.UploadState, &f.VerificationState,
		&postedAt, &nextCheck, &f.CleanupPolicy, &f.LastError, &f.CheckAttempts,
	); err != nil {
		return f, err
	}
	f.CompletedItemID = completed.String
	var err error
	if f.PostedAt, err = parseTimePtr(postedAt); err != nil {
		return f, err
	}
	if f.NextCheckAt, err = parseTimePtr(nextCheck); err != nil {
		return f, err
	}
	return f, nil
}

// GetFile returns a single transfer file, or sql.ErrNoRows if absent.
func (s *Store) GetFile(ctx context.Context, transferID, fileID string) (TransferFile, error) {
	row := s.db.QueryRowContext(ctx,
		"SELECT "+transferFileCols+" FROM transfer_files WHERE transfer_id = ? AND file_id = ?",
		transferID, fileID)
	return scanTransferFile(row)
}

// ListFilesByTransfer returns all files for a transfer.
func (s *Store) ListFilesByTransfer(ctx context.Context, transferID string) ([]TransferFile, error) {
	rows, err := s.db.QueryContext(ctx,
		"SELECT "+transferFileCols+" FROM transfer_files WHERE transfer_id = ? ORDER BY file_id", transferID)
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()

	var out []TransferFile
	for rows.Next() {
		f, err := scanTransferFile(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, f)
	}
	return out, rows.Err()
}

// SetVerificationState updates a file's verification state, next check time and
// last error in one statement.
func (s *Store) SetVerificationState(ctx context.Context, transferID, fileID, state string, nextCheckAt *time.Time, lastErr string) error {
	_, err := s.db.ExecContext(ctx, `
		UPDATE transfer_files
		SET verification_state = ?, next_check_at = ?, last_error = ?,
		    updated_at = ?
		WHERE transfer_id = ? AND file_id = ?`,
		state, fmtTimePtr(nextCheckAt), lastErr, fmtTime(time.Now()), transferID, fileID)
	return err
}

// DeferFileCheck reschedules a file's first verification check after a failed
// attempt (e.g. the manifest could not be read): it bumps check_attempts,
// pushes next_check_at out and records the error, leaving the file in the
// uploaded state so it is retried later instead of every poll cycle.
func (s *Store) DeferFileCheck(ctx context.Context, transferID, fileID string, nextCheckAt time.Time, lastErr string) error {
	_, err := s.db.ExecContext(ctx, `
		UPDATE transfer_files
		SET check_attempts = check_attempts + 1, next_check_at = ?, last_error = ?, updated_at = ?
		WHERE transfer_id = ? AND file_id = ?`,
		fmtTime(nextCheckAt), lastErr, fmtTime(time.Now()), transferID, fileID)
	return err
}

// MarkUploaded records that a file finished uploading: upload_state and
// verification_state both become "uploaded", posted_at is set, and next_check_at
// is when the first verification check is due (posted_at + post_check.delay).
func (s *Store) MarkUploaded(ctx context.Context, transferID, fileID string, postedAt, nextCheckAt time.Time) error {
	_, err := s.db.ExecContext(ctx, `
		UPDATE transfer_files
		SET upload_state = ?, verification_state = ?, posted_at = ?, next_check_at = ?, updated_at = ?
		WHERE transfer_id = ? AND file_id = ?`,
		StateUploaded, StateUploaded, fmtTime(postedAt), fmtTime(nextCheckAt), fmtTime(time.Now()),
		transferID, fileID)
	return err
}

// SetCleanupPolicy records the cleanup policy for a file (e.g. delete_original),
// read by the post-verification cleanup.
func (s *Store) SetCleanupPolicy(ctx context.Context, transferID, fileID, policy string) error {
	_, err := s.db.ExecContext(ctx,
		"UPDATE transfer_files SET cleanup_policy = ?, updated_at = ? WHERE transfer_id = ? AND file_id = ?",
		policy, fmtTime(time.Now()), transferID, fileID)
	return err
}

// SetCompletedItemForTransfer links a transfer's files to the completed_items
// row created for the upload, so verification status can be reflected back.
func (s *Store) SetCompletedItemForTransfer(ctx context.Context, transferID, completedItemID string) error {
	_, err := s.db.ExecContext(ctx,
		"UPDATE transfer_files SET completed_item_id = ?, updated_at = ? WHERE transfer_id = ?",
		completedItemID, fmtTime(time.Now()), transferID)
	return err
}

// SetCompletedItemVerificationStatus updates the verification_status of a
// completed item (the user-facing queue row) to reflect durable verification.
func (s *Store) SetCompletedItemVerificationStatus(ctx context.Context, completedItemID, status string) error {
	if completedItemID == "" {
		return nil
	}
	_, err := s.db.ExecContext(ctx,
		"UPDATE completed_items SET verification_status = ? WHERE id = ?", status, completedItemID)
	return err
}

// MigrateLegacyPendingChecks moves any rows from the pre-durable
// pending_article_checks table into verification_failures as STAT-only records
// (keyed by transfer_id=completed_item_id, file_id=legacy, article_index=-1),
// so the durable verification service continues checking them. The legacy rows
// are deleted in the same transaction. Idempotent (returns 0 once empty), and a
// no-op when the legacy table does not exist.
func (s *Store) MigrateLegacyPendingChecks(ctx context.Context) (int, error) {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return 0, err
	}
	defer func() { _ = tx.Rollback() }()

	rows, err := tx.QueryContext(ctx,
		"SELECT completed_item_id, message_id, groups, next_retry_at, retry_count FROM pending_article_checks")
	if err != nil {
		// Legacy table absent (fresh install) — nothing to migrate.
		return 0, nil
	}

	type legacy struct {
		completedItemID, messageID, groups, nextRetryAt string
		retryCount                                      int
	}
	var pending []legacy
	for rows.Next() {
		var l legacy
		if err := rows.Scan(&l.completedItemID, &l.messageID, &l.groups, &l.nextRetryAt, &l.retryCount); err != nil {
			_ = rows.Close()
			return 0, err
		}
		pending = append(pending, l)
	}
	if err := rows.Err(); err != nil {
		_ = rows.Close()
		return 0, err
	}
	_ = rows.Close()

	for _, l := range pending {
		nextAt, perr := parseTime(l.nextRetryAt)
		if perr != nil {
			nextAt = time.Now()
		}
		if _, err := tx.ExecContext(ctx, `
			INSERT INTO verification_failures
				(transfer_id, file_id, article_index, message_id, groups, deferred_count, state, next_attempt_at)
			VALUES (?,?,?,?,?,?,?,?)
			ON CONFLICT(transfer_id, file_id, message_id) DO NOTHING`,
			l.completedItemID, LegacyFileID, -1, l.messageID, l.groups, l.retryCount, FailurePending, fmtTime(nextAt)); err != nil {
			return 0, fmt.Errorf("migrate legacy check: %w", err)
		}
	}

	if _, err := tx.ExecContext(ctx, "DELETE FROM pending_article_checks"); err != nil {
		return 0, err
	}
	if err := tx.Commit(); err != nil {
		return 0, err
	}
	return len(pending), nil
}

// GetCompletedItemNZBPath returns the NZB path recorded for a completed item,
// used by post-verification cleanup to run the post-upload script.
func (s *Store) GetCompletedItemNZBPath(ctx context.Context, completedItemID string) (string, error) {
	if completedItemID == "" {
		return "", nil
	}
	var nzbPath string
	err := s.db.QueryRowContext(ctx, "SELECT nzb_path FROM completed_items WHERE id = ?", completedItemID).Scan(&nzbPath)
	if err != nil {
		return "", err
	}
	return nzbPath, nil
}

// DeleteFilesByTransfer removes all transfer_files rows for a transfer, used
// after post-verification cleanup completes.
func (s *Store) DeleteFilesByTransfer(ctx context.Context, transferID string) error {
	_, err := s.db.ExecContext(ctx, "DELETE FROM transfer_files WHERE transfer_id = ?", transferID)
	return err
}

// DeleteFailuresByTransfer removes every verification failure recorded for a
// transfer, used when the transfer is discarded before verification.
func (s *Store) DeleteFailuresByTransfer(ctx context.Context, transferID string) error {
	_, err := s.db.ExecContext(ctx, "DELETE FROM verification_failures WHERE transfer_id = ?", transferID)
	return err
}

// ListDueFiles returns files awaiting their first verification check
// (verification_state = uploaded, next_check_at <= now), oldest first.
func (s *Store) ListDueFiles(ctx context.Context, now time.Time, limit int) ([]TransferFile, error) {
	rows, err := s.db.QueryContext(ctx,
		"SELECT "+transferFileCols+` FROM transfer_files
		 WHERE verification_state = ? AND next_check_at IS NOT NULL AND next_check_at <= ?
		 ORDER BY next_check_at LIMIT ?`,
		StateUploaded, fmtTime(now), limit)
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()

	var out []TransferFile
	for rows.Next() {
		f, err := scanTransferFile(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, f)
	}
	return out, rows.Err()
}

// SetUploadState updates a file's upload state and (optionally) posted_at.
func (s *Store) SetUploadState(ctx context.Context, transferID, fileID, state string, postedAt *time.Time) error {
	_, err := s.db.ExecContext(ctx, `
		UPDATE transfer_files
		SET upload_state = ?, posted_at = COALESCE(?, posted_at), updated_at = ?
		WHERE transfer_id = ? AND file_id = ?`,
		state, fmtTimePtr(postedAt), fmtTime(time.Now()), transferID, fileID)
	return err
}

// AddFailure records a failed article for later re-posting/re-checking. Records
// are unique per (transfer_id, file_id, message_id); duplicates are ignored.
func (s *Store) AddFailure(ctx context.Context, f VerificationFailure) error {
	if f.State == "" {
		f.State = FailurePending
	}
	if f.NextAttemptAt.IsZero() {
		f.NextAttemptAt = time.Now()
	}
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO verification_failures (
			transfer_id, file_id, article_index, message_id, groups,
			repost_count, deferred_count, state, next_attempt_at, last_error
		) VALUES (?,?,?,?,?,?,?,?,?,?)
		ON CONFLICT(transfer_id, file_id, message_id) DO NOTHING`,
		f.TransferID, f.FileID, f.ArticleIndex, f.MessageID, encodeGroups(f.Groups),
		f.RepostCount, f.DeferredCount, f.State, fmtTime(f.NextAttemptAt), f.LastError,
	)
	if err != nil {
		return fmt.Errorf("add verification failure: %w", err)
	}
	return nil
}

const failureCols = `id, transfer_id, file_id, article_index, message_id, groups,
	repost_count, deferred_count, state, next_attempt_at, lease_owner, lease_expires_at,
	last_error, last_checked_at`

func scanFailure(sc interface{ Scan(...any) error }) (VerificationFailure, error) {
	var (
		f         VerificationFailure
		groups    string
		leaseExp  sql.NullString
		lastCheck sql.NullString
		nextAt    string
	)
	if err := sc.Scan(
		&f.ID, &f.TransferID, &f.FileID, &f.ArticleIndex, &f.MessageID, &groups,
		&f.RepostCount, &f.DeferredCount, &f.State, &nextAt, &f.LeaseOwner, &leaseExp,
		&f.LastError, &lastCheck,
	); err != nil {
		return f, err
	}
	f.Groups = decodeGroups(groups)
	var err error
	if f.NextAttemptAt, err = parseTime(nextAt); err != nil {
		return f, err
	}
	if f.LeaseExpiresAt, err = parseTimePtr(leaseExp); err != nil {
		return f, err
	}
	if f.LastCheckedAt, err = parseTimePtr(lastCheck); err != nil {
		return f, err
	}
	return f, nil
}

// ClaimDueFailures atomically leases up to limit pending failures whose
// next_attempt_at has passed and whose lease is free or expired, marking them
// owned by owner until now+leaseDur. The claimed rows are returned. Using a
// transaction makes the select-then-update atomic so concurrent workers (or
// instances) never claim the same row.
func (s *Store) ClaimDueFailures(ctx context.Context, owner string, leaseDur time.Duration, limit int, now time.Time) ([]VerificationFailure, error) {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, err
	}
	defer func() { _ = tx.Rollback() }()

	nowStr := fmtTime(now)
	rows, err := tx.QueryContext(ctx, `
		SELECT `+failureCols+`
		FROM verification_failures
		WHERE state = ?
		  AND next_attempt_at <= ?
		  AND (lease_owner = '' OR lease_expires_at IS NULL OR lease_expires_at <= ?)
		ORDER BY next_attempt_at
		LIMIT ?`,
		FailurePending, nowStr, nowStr, limit)
	if err != nil {
		return nil, err
	}

	var claimed []VerificationFailure
	for rows.Next() {
		f, scanErr := scanFailure(rows)
		if scanErr != nil {
			_ = rows.Close()
			return nil, scanErr
		}
		claimed = append(claimed, f)
	}
	if err := rows.Err(); err != nil {
		_ = rows.Close()
		return nil, err
	}
	_ = rows.Close()

	if len(claimed) == 0 {
		return nil, tx.Commit()
	}

	expires := fmtTime(now.Add(leaseDur))
	for i := range claimed {
		if _, err := tx.ExecContext(ctx,
			"UPDATE verification_failures SET lease_owner = ?, lease_expires_at = ? WHERE id = ?",
			owner, expires, claimed[i].ID); err != nil {
			return nil, err
		}
		claimed[i].LeaseOwner = owner
		exp, _ := parseTime(expires)
		claimed[i].LeaseExpiresAt = &exp
	}

	if err := tx.Commit(); err != nil {
		return nil, err
	}
	return claimed, nil
}

// UpdateFailureAfterCheck records the outcome of a check/re-post attempt:
// the new state, counts, next attempt time and last error, and releases the
// lease.
func (s *Store) UpdateFailureAfterCheck(ctx context.Context, f VerificationFailure) error {
	_, err := s.db.ExecContext(ctx, `
		UPDATE verification_failures
		SET state = ?, repost_count = ?, deferred_count = ?, next_attempt_at = ?,
		    last_error = ?, last_checked_at = ?, lease_owner = '', lease_expires_at = NULL
		WHERE id = ?`,
		f.State, f.RepostCount, f.DeferredCount, fmtTime(f.NextAttemptAt),
		f.LastError, fmtTime(time.Now()), f.ID)
	return err
}

// ReclaimExpiredLeases clears leases whose expiry has passed so the work can be
// picked up again after a crash. Returns the number of rows reclaimed.
func (s *Store) ReclaimExpiredLeases(ctx context.Context, now time.Time) (int64, error) {
	res, err := s.db.ExecContext(ctx, `
		UPDATE verification_failures
		SET lease_owner = '', lease_expires_at = NULL
		WHERE lease_owner != '' AND lease_expires_at IS NOT NULL AND lease_expires_at <= ?`,
		fmtTime(now))
	if err != nil {
		return 0, err
	}
	return res.RowsAffected()
}

// ListPendingVerificationItemIDs returns the ids of completed items whose
// verification_status is still pending_verification, used by startup
// reconciliation to repair items that can no longer be finalized by the
// normal verification flow (orphans, legacy upgrades).
func (s *Store) ListPendingVerificationItemIDs(ctx context.Context) ([]string, error) {
	rows, err := s.db.QueryContext(ctx,
		"SELECT id FROM completed_items WHERE verification_status = 'pending_verification'")
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()

	var ids []string
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			return nil, err
		}
		ids = append(ids, id)
	}
	return ids, rows.Err()
}

// ListFilesByCompletedItem returns all transfer files linked to a completed
// item.
func (s *Store) ListFilesByCompletedItem(ctx context.Context, completedItemID string) ([]TransferFile, error) {
	rows, err := s.db.QueryContext(ctx,
		"SELECT "+transferFileCols+" FROM transfer_files WHERE completed_item_id = ? ORDER BY file_id",
		completedItemID)
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()

	var out []TransferFile
	for rows.Next() {
		f, err := scanTransferFile(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, f)
	}
	return out, rows.Err()
}

// CountFailures returns the number of failure rows for a transfer file in the
// given state ("" = any state).
func (s *Store) CountFailures(ctx context.Context, transferID, fileID, state string) (int, error) {
	q := "SELECT COUNT(*) FROM verification_failures WHERE transfer_id = ? AND file_id = ?"
	args := []any{transferID, fileID}
	if state != "" {
		q += " AND state = ?"
		args = append(args, state)
	}
	var n int
	err := s.db.QueryRowContext(ctx, q, args...).Scan(&n)
	return n, err
}
