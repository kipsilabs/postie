package queue

import (
	"context"
	"fmt"
	"path/filepath"
	"testing"
	"time"

	"github.com/javi11/postie/internal/config"
	"github.com/javi11/postie/internal/database"
)

// newTestQueue creates an isolated Queue backed by a temp sqlite DB with all
// migrations applied. The DB is removed automatically when the test ends.
func newTestQueue(t *testing.T) *Queue {
	t.Helper()
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "test.db")

	ctx := context.Background()
	db, err := database.New(ctx, config.DatabaseConfig{
		DatabaseType: "sqlite",
		DatabasePath: dbPath,
	})
	if err != nil {
		t.Fatalf("database.New: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })

	if err := db.GetMigrationRunner().MigrateUp(); err != nil {
		t.Fatalf("MigrateUp: %v", err)
	}

	q, err := New(ctx, db)
	if err != nil {
		t.Fatalf("queue.New: %v", err)
	}
	t.Cleanup(func() { _ = q.Close() })
	return q
}

func countRows(t *testing.T, q *Queue, table, where string, args ...any) int {
	t.Helper()
	var n int
	query := "SELECT COUNT(*) FROM " + table
	if where != "" {
		query += " WHERE " + where
	}
	if err := q.db.QueryRow(query, args...).Scan(&n); err != nil {
		t.Fatalf("count %s: %v", table, err)
	}
	return n
}

// TestRetryDoesNotLeakInProgressRows reproduces the duplicate-entries bug.
// Without ClearInProgress before ReaddJob, each retry leaks an in_progress
// row keyed by the previous goqite message ID, causing the same path to
// appear in two tables simultaneously (pending goqite + stale in_progress).
func TestRetryDoesNotLeakInProgressRows(t *testing.T) {
	q := newTestQueue(t)
	ctx := context.Background()

	const path = "/tmp/example.bin"
	if err := q.AddFile(ctx, path, 1234); err != nil {
		t.Fatalf("AddFile: %v", err)
	}

	// Simulate 3 receive→fail→retry cycles, mirroring processor.handleProcessingError.
	for i := range 3 {
		msg, job, err := q.ReceiveFile(ctx)
		if err != nil {
			t.Fatalf("ReceiveFile #%d: %v", i, err)
		}
		if msg == nil || job == nil {
			t.Fatalf("ReceiveFile #%d returned nil", i)
		}

		// Invariant: while processing, exactly one in_progress row for this path.
		if got := countRows(t, q, "in_progress_items", "path = ?", path); got != 1 {
			t.Fatalf("cycle %d: in_progress rows = %d, want 1", i, got)
		}

		// Simulate the retry path in handleProcessingError.
		if err := q.ClearInProgress(ctx, msg.ID); err != nil {
			t.Fatalf("ClearInProgress: %v", err)
		}
		job.RetryCount++
		if err := q.ReaddJob(ctx, job); err != nil {
			t.Fatalf("ReaddJob: %v", err)
		}

		// After clear+readd: exactly one pending goqite row, zero in_progress rows.
		if got := countRows(t, q, "in_progress_items", "path = ?", path); got != 0 {
			t.Fatalf("cycle %d: in_progress leak after ClearInProgress: %d rows", i, got)
		}
		if got := countRows(t, q, "goqite", "queue = 'file_jobs' AND json_extract(body, '$.path') = ?", path); got != 1 {
			t.Fatalf("cycle %d: pending goqite rows = %d, want 1", i, got)
		}
	}
}

// TestTransferIDAssignedAndStableAcrossRetry verifies every queued job gets a
// stable transfer_id at creation that is preserved across receive→readd retry
// cycles, so the durable upload architecture can reuse the same manifest.
func TestTransferIDAssignedAndStableAcrossRetry(t *testing.T) {
	q := newTestQueue(t)
	ctx := context.Background()

	const path = "/tmp/transfer.bin"
	if err := q.AddFile(ctx, path, 1234); err != nil {
		t.Fatalf("AddFile: %v", err)
	}

	msg, job, err := q.ReceiveFile(ctx)
	if err != nil || job == nil {
		t.Fatalf("ReceiveFile: %v", err)
	}
	if job.TransferID == "" {
		t.Fatal("TransferID empty after AddFile/ReceiveFile")
	}
	originalID := job.TransferID

	// Retry cycle preserves the transfer id.
	if err := q.ClearInProgress(ctx, msg.ID); err != nil {
		t.Fatalf("ClearInProgress: %v", err)
	}
	job.RetryCount++
	if err := q.ReaddJob(ctx, job); err != nil {
		t.Fatalf("ReaddJob: %v", err)
	}

	_, job2, err := q.ReceiveFile(ctx)
	if err != nil || job2 == nil {
		t.Fatalf("ReceiveFile after readd: %v", err)
	}
	if job2.TransferID != originalID {
		t.Errorf("TransferID changed across retry: %q -> %q", originalID, job2.TransferID)
	}
}

// TestIsPathInQueueDuringReceive verifies the path is always visible to
// IsPathInQueue during ReceiveFile (insert-then-delete ordering).
func TestIsPathInQueueDuringReceive(t *testing.T) {
	q := newTestQueue(t)
	ctx := context.Background()

	const path = "/tmp/race.bin"
	if err := q.AddFile(ctx, path, 100); err != nil {
		t.Fatalf("AddFile: %v", err)
	}

	if ok, err := q.IsPathInQueue(path); err != nil || !ok {
		t.Fatalf("before receive: IsPathInQueue=%v err=%v, want true,nil", ok, err)
	}

	msg, _, err := q.ReceiveFile(ctx)
	if err != nil || msg == nil {
		t.Fatalf("ReceiveFile: msg=%v err=%v", msg, err)
	}

	if ok, err := q.IsPathInQueue(path); err != nil || !ok {
		t.Fatalf("after receive (in_progress): IsPathInQueue=%v err=%v, want true,nil", ok, err)
	}

	if err := q.ClearInProgress(ctx, msg.ID); err != nil {
		t.Fatalf("ClearInProgress: %v", err)
	}

	if ok, err := q.IsPathInQueue(path); err != nil || ok {
		t.Fatalf("after clear: IsPathInQueue=%v err=%v, want false,nil", ok, err)
	}
}

// TestAddManualFilePinsDeleteOriginalFalse verifies that a manual (user-initiated)
// upload pins the per-job DeleteOriginal override to false so the original file is
// preserved regardless of the watcher's delete_original_file policy. A plain
// AddFile (used by the watcher path) must leave the override unset (nil) so it
// keeps honouring the global setting.
func TestAddManualFilePinsDeleteOriginalFalse(t *testing.T) {
	q := newTestQueue(t)
	ctx := context.Background()

	const manualPath = "/tmp/manual.bin"
	if err := q.AddManualFile(ctx, manualPath, 100); err != nil {
		t.Fatalf("AddManualFile: %v", err)
	}

	_, job, err := q.ReceiveFile(ctx)
	if err != nil || job == nil {
		t.Fatalf("ReceiveFile: job=%v err=%v", job, err)
	}
	if job.DeleteOriginal == nil {
		t.Fatal("manual upload: DeleteOriginal override is nil, want non-nil false")
	}
	if *job.DeleteOriginal {
		t.Errorf("manual upload: DeleteOriginal = true, want false")
	}

	// Contrast: the plain AddFile path (watcher/automatic) leaves the override
	// unset so the processor falls back to the global config value.
	const autoPath = "/tmp/auto.bin"
	if err := q.AddFile(ctx, autoPath, 100); err != nil {
		t.Fatalf("AddFile: %v", err)
	}
	_, autoJob, err := q.ReceiveFile(ctx)
	if err != nil || autoJob == nil {
		t.Fatalf("ReceiveFile auto: job=%v err=%v", autoJob, err)
	}
	if autoJob.DeleteOriginal != nil {
		t.Errorf("automatic upload: DeleteOriginal override = %v, want nil", *autoJob.DeleteOriginal)
	}
}

func TestGetQueueStats_VerificationCounts(t *testing.T) {
	q := newTestQueue(t)
	ctx := context.Background()

	insert := func(id, status string) {
		if _, err := q.db.ExecContext(ctx, `INSERT INTO completed_items
			(id, path, size, nzb_path, created_at, job_data, verification_status)
			VALUES (?,?,?,?,?,?,?)`,
			id, "/d/"+id, 1, "/o/"+id+".nzb", "2026-01-01T00:00:00Z", []byte("{}"), status); err != nil {
			t.Fatalf("insert %s: %v", id, err)
		}
	}
	insert("a", "verified")
	insert("b", "pending_verification")
	insert("c", "pending_verification")
	insert("d", "verification_failed")

	stats, err := q.GetQueueStats()
	if err != nil {
		t.Fatalf("GetQueueStats: %v", err)
	}
	if got := stats["pendingVerification"]; got != 2 {
		t.Errorf("pendingVerification = %v, want 2", got)
	}
	if got := stats["verificationFailed"]; got != 1 {
		t.Errorf("verificationFailed = %v, want 1", got)
	}
}

func TestAttachVerificationProgress(t *testing.T) {
	q := newTestQueue(t)
	ctx := context.Background()

	insertItem := func(id, status string) {
		if _, err := q.db.ExecContext(ctx, `INSERT INTO completed_items
			(id, path, size, nzb_path, created_at, job_data, verification_status)
			VALUES (?,?,?,?,?,?,?)`,
			id, "/d/"+id, 1, "/o/"+id+".nzb", "2026-01-01T00:00:00Z", []byte("{}"), status); err != nil {
			t.Fatalf("insert %s: %v", id, err)
		}
	}
	addChecks := func(itemID string, count int) {
		checks := make([]PendingArticleCheck, count)
		for i := range checks {
			checks[i] = PendingArticleCheck{
				MessageID:   fmt.Sprintf("<%s-%d@test>", itemID, i),
				Groups:      `["alt.test"]`,
				NextRetryAt: time.Now(),
			}
		}
		if err := q.AddPendingArticleChecks(ctx, itemID, checks); err != nil {
			t.Fatalf("AddPendingArticleChecks %s: %v", itemID, err)
		}
	}

	insertTransferFile := func(itemID, fileID, state string, articleCount int) {
		if _, err := q.db.ExecContext(ctx, `INSERT INTO transfer_files
			(transfer_id, file_id, completed_item_id, manifest_path, source_path, article_count, upload_state, verification_state)
			VALUES (?,?,?,?,?,?,?,?)`,
			"tr-"+itemID, fileID, itemID, "/m/"+fileID, "/s/"+fileID, articleCount, "uploaded", state); err != nil {
			t.Fatalf("insert transfer_file %s/%s: %v", itemID, fileID, err)
		}
	}

	insertFailure := func(itemID, fileID string, n int, state string) {
		for i := 0; i < n; i++ {
			if _, err := q.db.ExecContext(ctx, `INSERT INTO verification_failures
				(transfer_id, file_id, message_id, state)
				VALUES (?,?,?,?)`,
				"tr-"+itemID, fileID, fmt.Sprintf("<vf-%s-%s-%s-%d@test>", itemID, fileID, state, i), state); err != nil {
				t.Fatalf("insert failure %s/%s: %v", itemID, fileID, err)
			}
		}
	}

	// Item "a": legacy path — 4 articles in pending_article_checks, 3 verified.
	// Item "b": legacy path — 2 articles, all pending.
	// Item "c": pending_verification but no rows anywhere (edge case).
	// Item "d": already verified — must not be touched.
	// Item "f": durable path — f1 fully verified (10), f2 verifying with 30
	// articles of which 25 are still open failures and 5 resolved → 15/40.
	insertItem("a", "pending_verification")
	insertItem("b", "pending_verification")
	insertItem("c", "pending_verification")
	insertItem("d", "verified")
	insertItem("f", "pending_verification")
	addChecks("a", 4)
	addChecks("b", 2)
	insertTransferFile("f", "f1", "verified", 10)
	insertTransferFile("f", "f2", "verifying", 30)
	insertFailure("f", "f2", 25, "pending")
	insertFailure("f", "f2", 5, "resolved")
	if _, err := q.db.ExecContext(ctx, `UPDATE pending_article_checks
		SET status = 'verified'
		WHERE completed_item_id = 'a' AND id IN (
			SELECT id FROM pending_article_checks WHERE completed_item_id = 'a' LIMIT 3
		)`); err != nil {
		t.Fatalf("mark verified: %v", err)
	}

	pending := "pending_verification"
	verified := "verified"
	items := []QueueItem{
		{ID: "a", VerificationStatus: &pending},
		{ID: "b", VerificationStatus: &pending},
		{ID: "c", VerificationStatus: &pending},
		{ID: "d", VerificationStatus: &verified},
		{ID: "e"}, // no verification status at all
		{ID: "f", VerificationStatus: &pending},
	}

	q.attachVerificationProgress(items)

	check := func(idx int, wantVerified, wantTotal int) {
		t.Helper()
		item := items[idx]
		if item.TotalArticles == nil || item.VerifiedArticles == nil {
			t.Fatalf("item %s: progress not attached", item.ID)
		}
		if *item.VerifiedArticles != wantVerified || *item.TotalArticles != wantTotal {
			t.Errorf("item %s: progress = %d/%d, want %d/%d",
				item.ID, *item.VerifiedArticles, *item.TotalArticles, wantVerified, wantTotal)
		}
	}
	check(0, 3, 4)
	check(1, 0, 2)
	check(5, 15, 40)
	for _, idx := range []int{2, 3, 4} {
		if items[idx].TotalArticles != nil || items[idx].VerifiedArticles != nil {
			t.Errorf("item %s: expected no progress attached", items[idx].ID)
		}
	}

	// Empty slice must be a no-op.
	q.attachVerificationProgress(nil)

	// End-to-end: GetQueueItems must return items with progress attached.
	result, err := q.GetQueueItems(PaginationParams{Status: "complete", Page: 1, Limit: 25})
	if err != nil {
		t.Fatalf("GetQueueItems: %v", err)
	}
	found := false
	for _, item := range result.Items {
		if item.ID == "a" {
			found = true
			if item.TotalArticles == nil || *item.TotalArticles != 4 ||
				item.VerifiedArticles == nil || *item.VerifiedArticles != 3 {
				t.Errorf("GetQueueItems item a: progress not attached correctly: %+v", item)
			}
		}
	}
	if !found {
		t.Error("GetQueueItems did not return completed item a")
	}
}

// TestGetQueueItems_PendingFilterSortsByCreated reproduces issue #245 (1):
// filtering by status=pending with the default "created" sort produced
// "no such column: created_at" because the goqite table names that column
// "created". The endpoint returned HTTP 500 and the dashboard queue was empty.
func TestGetQueueItems_PendingFilterSortsByCreated(t *testing.T) {
	q := newTestQueue(t)
	ctx := context.Background()

	if err := q.AddFile(ctx, "/tmp/pending-a.bin", 10); err != nil {
		t.Fatalf("AddFile a: %v", err)
	}
	if err := q.AddFile(ctx, "/tmp/pending-b.bin", 20); err != nil {
		t.Fatalf("AddFile b: %v", err)
	}

	for _, order := range []string{"", "asc", "desc"} {
		result, err := q.GetQueueItems(PaginationParams{Status: "pending", SortBy: "created", Order: order, Page: 1, Limit: 25})
		if err != nil {
			t.Fatalf("GetQueueItems(status=pending, order=%q): %v", order, err)
		}
		if result.TotalItems != 2 || len(result.Items) != 2 {
			t.Fatalf("order=%q: got total=%d items=%d, want 2/2", order, result.TotalItems, len(result.Items))
		}
		for _, item := range result.Items {
			if item.Status != StatusPending {
				t.Errorf("order=%q: item %s status=%q, want pending", order, item.Path, item.Status)
			}
		}
	}
}

// TestGetQueueItems_StatusSortPagesAreContiguous guards the offset arithmetic
// in getMergedItemsByStatus. When a page offset exceeds a status bucket, the
// offset must shrink by that bucket's total size, not by the (empty) page it
// returned, otherwise items at the bucket boundary are silently skipped.
func TestGetQueueItems_StatusSortPagesAreContiguous(t *testing.T) {
	q := newTestQueue(t)
	ctx := context.Background()

	// One running item, three pending items.
	if err := q.AddFile(ctx, "/tmp/running.bin", 1); err != nil {
		t.Fatalf("AddFile running: %v", err)
	}
	if msg, _, err := q.ReceiveFile(ctx); err != nil || msg == nil {
		t.Fatalf("ReceiveFile: msg=%v err=%v", msg, err)
	}
	for _, name := range []string{"p1", "p2", "p3"} {
		if err := q.AddFile(ctx, "/tmp/"+name+".bin", 1); err != nil {
			t.Fatalf("AddFile %s: %v", name, err)
		}
	}

	seen := map[string]int{}
	for page := 1; page <= 2; page++ {
		res, err := q.GetQueueItems(PaginationParams{SortBy: "status", Order: "desc", Page: page, Limit: 2})
		if err != nil {
			t.Fatalf("page %d: %v", page, err)
		}
		if len(res.Items) != 2 {
			t.Fatalf("page %d: got %d items, want 2 (%+v)", page, len(res.Items), res.Items)
		}
		for _, it := range res.Items {
			seen[it.Path]++
		}
	}
	for _, path := range []string{"/tmp/running.bin", "/tmp/p1.bin", "/tmp/p2.bin", "/tmp/p3.bin"} {
		if seen[path] != 1 {
			t.Errorf("%s appeared %d times across pages, want exactly once", path, seen[path])
		}
	}
}
