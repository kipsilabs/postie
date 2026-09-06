package verification

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/database"
	"github.com/kipsilabs/postie/internal/manifest"
	"github.com/kipsilabs/postie/internal/transferstore"
)

// fakeStater returns "missing" for any message id in the missing set, and a
// transient check error for any id in the errored set.
type fakeStater struct {
	mu      sync.Mutex
	missing map[string]bool
	errored map[string]bool
	calls   map[string]int
}

func newFakeStater(missing ...string) *fakeStater {
	m := make(map[string]bool)
	for _, id := range missing {
		m[id] = true
	}
	return &fakeStater{missing: m, errored: make(map[string]bool), calls: make(map[string]int)}
}

func (f *fakeStater) Stat(_ context.Context, messageID string) (bool, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls[messageID]++
	if f.errored[messageID] {
		return false, errors.New("stat timeout")
	}
	if f.missing[messageID] {
		return true, nil
	}
	return false, nil
}

func (f *fakeStater) markErrored(id string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.errored[id] = true
}

func (f *fakeStater) clearErrored(id string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	delete(f.errored, id)
}

func (f *fakeStater) StatBatch(ctx context.Context, messageIDs []string) (map[string]struct{}, error) {
	missing := make(map[string]struct{})
	for _, id := range messageIDs {
		if isMissing, err := f.Stat(ctx, id); err != nil || isMissing {
			missing[id] = struct{}{}
		}
	}
	return missing, nil
}

func (f *fakeStater) markPresent(id string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	delete(f.missing, id)
}

// fakeReposter records reposts and can optionally mark them present afterwards.
type fakeReposter struct {
	mu       sync.Mutex
	reposted []string
	onRepost func(rec manifest.ArticleRecord)
	failWith error
}

func (f *fakeReposter) Repost(_ context.Context, rec manifest.ArticleRecord) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.failWith != nil {
		return f.failWith
	}
	f.reposted = append(f.reposted, rec.MessageID)
	if f.onRepost != nil {
		f.onRepost(rec)
	}
	return nil
}

func (f *fakeReposter) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.reposted)
}

func newTestStore(t *testing.T) *transferstore.Store {
	t.Helper()
	store, _ := newTestStoreWithDB(t)
	return store
}

func newTestStoreWithDB(t *testing.T) (*transferstore.Store, *sql.DB) {
	t.Helper()
	ctx := context.Background()
	db, err := database.New(ctx, config.DatabaseConfig{
		DatabaseType: "sqlite",
		DatabasePath: filepath.Join(t.TempDir(), "v.db"),
	})
	if err != nil {
		t.Fatalf("database.New: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	if err := db.GetMigrationRunner().MigrateUp(); err != nil {
		t.Fatalf("MigrateUp: %v", err)
	}
	return transferstore.New(db.DB), db.DB
}

// writeManifest creates a manifest with n articles "<m0>".."<m{n-1}>" and
// registers a transfer_files row pointing at it.
func writeManifest(t *testing.T, store *transferstore.Store, transferID, fileID string, n int) string {
	t.Helper()
	path := manifest.FilePath(t.TempDir(), transferID, fileID)
	w, err := manifest.NewWriter(path)
	if err != nil {
		t.Fatalf("NewWriter: %v", err)
	}
	for i := 0; i < n; i++ {
		if err := w.Write(manifest.ArticleRecord{
			Index:     i,
			MessageID: mid(i),
			Offset:    int64(i) * 1000,
			BodySize:  1000,
			Groups:    []string{"alt.binaries.test"},
		}); err != nil {
			t.Fatalf("Write: %v", err)
		}
	}
	if err := w.Commit(); err != nil {
		t.Fatalf("Commit: %v", err)
	}
	if err := store.UpsertFile(context.Background(), transferstore.TransferFile{
		TransferID:   transferID,
		FileID:       fileID,
		ManifestPath: path,
		SourcePath:   "/data/x.bin",
		FileRole:     "original",
		ArticleCount: n,
		UploadState:  transferstore.StateUploaded,
	}); err != nil {
		t.Fatalf("UpsertFile: %v", err)
	}
	return path
}

func mid(i int) string { return "<m" + string(rune('0'+i)) + ">" }

func TestVerifyFile_AllPresentMarksVerified(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 5)

	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}

	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerified {
		t.Errorf("state = %q, want verified", got.VerificationState)
	}
	if n, _ := store.CountFailures(ctx, "t", "f", ""); n != 0 {
		t.Errorf("failures = %d, want 0", n)
	}
}

func TestVerifyFile_PersistsMisses(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 5)

	// m1 and m3 missing.
	svc := New(store, newFakeStater(mid(1), mid(3)), &fakeReposter{}, Config{}, "w")
	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}

	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerifying {
		t.Errorf("state = %q, want verifying", got.VerificationState)
	}
	if n, _ := store.CountFailures(ctx, "t", "f", transferstore.FailurePending); n != 2 {
		t.Errorf("pending failures = %d, want 2", n)
	}
}

func TestProcessDueFailures_RepostThenResolve(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 3)

	stater := newFakeStater(mid(1))
	// When re-posted, the article becomes present on the next STAT.
	reposter := &fakeReposter{onRepost: func(rec manifest.ArticleRecord) { stater.markPresent(rec.MessageID) }}

	cfg := Config{MaxReposts: 1, PropagationDelay: time.Millisecond, DeferredBackoff: time.Millisecond}
	svc := New(store, stater, reposter, cfg, "w")

	// Initial verification persists the miss for m1.
	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}

	now := time.Now().Add(time.Second) // make the failure due
	// First pass: re-posts m1 (RepostCount 0 -> 1), still pending.
	if _, err := svc.ProcessDueFailures(ctx, now); err != nil {
		t.Fatalf("ProcessDueFailures #1: %v", err)
	}
	if reposter.count() != 1 {
		t.Errorf("reposts = %d, want 1", reposter.count())
	}

	// Second pass (after the scheduled recheck is due): RepostCount now == MaxReposts,
	// so STAT-only recheck runs and finds it present -> resolved -> file verified.
	now = now.Add(time.Hour)
	if _, err := svc.ProcessDueFailures(ctx, now); err != nil {
		t.Fatalf("ProcessDueFailures #2: %v", err)
	}

	if n, _ := store.CountFailures(ctx, "t", "f", transferstore.FailurePending); n != 0 {
		t.Errorf("pending = %d, want 0", n)
	}
	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerified {
		t.Errorf("state = %q, want verified", got.VerificationState)
	}
}

// TestProcessDueFailures_PropagationLagResolvesWithoutRepost ensures that an
// article recorded missing during the initial sweep but present by the time
// its failure is processed (provider propagation lag) is resolved by the
// confirming STAT and never re-posted. Without the STAT-first rule, a slow
// provider caused the entire upload to be re-posted.
func TestProcessDueFailures_PropagationLagResolvesWithoutRepost(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 3)

	stater := newFakeStater(mid(0), mid(1), mid(2)) // everything "missing" at sweep time
	reposter := &fakeReposter{}

	cfg := Config{MaxReposts: 3, PropagationDelay: time.Millisecond, DeferredBackoff: time.Millisecond}
	svc := New(store, stater, reposter, cfg, "w")

	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}
	if n, _ := store.CountFailures(ctx, "t", "f", transferstore.FailurePending); n != 3 {
		t.Fatalf("pending after sweep = %d, want 3", n)
	}

	// Articles propagate before the failures are processed.
	for i := 0; i < 3; i++ {
		stater.markPresent(mid(i))
	}

	if _, err := svc.ProcessDueFailures(ctx, time.Now().Add(time.Second)); err != nil {
		t.Fatalf("ProcessDueFailures: %v", err)
	}

	if reposter.count() != 0 {
		t.Errorf("reposts = %d, want 0 (propagation lag must not trigger re-uploads)", reposter.count())
	}
	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerified {
		t.Errorf("file state = %q, want verified", got.VerificationState)
	}
}

// TestProcessDueFailures_RepostSourceGoneFallsBackToStat reproduces the
// "stuck verifying forever" bug: when the source file for a re-post has been
// deleted (e.g. temp PAR2 cleaned up), the repost error must not be retried
// unboundedly — the failure must fall through to the bounded STAT-recheck
// path and eventually terminate.
func TestProcessDueFailures_RepostSourceGoneFallsBackToStat(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 2)

	stater := newFakeStater(mid(1)) // m1 permanently missing
	reposter := &fakeReposter{failWith: fmt.Errorf("repost: open source %q: %w", "/tmp/gone.par2", fs.ErrNotExist)}

	cfg := Config{
		MaxReposts:        3,
		PropagationDelay:  time.Millisecond,
		DeferredBackoff:   time.Millisecond,
		MaxDeferredChecks: 2,
	}
	svc := New(store, stater, reposter, cfg, "w")

	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}

	// Each pass must consume a deferred check instead of looping on repost.
	now := time.Now().Add(time.Second)
	for i := 0; i < cfg.MaxDeferredChecks; i++ {
		if _, err := svc.ProcessDueFailures(ctx, now); err != nil {
			t.Fatalf("ProcessDueFailures #%d: %v", i+1, err)
		}
		now = now.Add(time.Hour)
	}

	if n, _ := store.CountFailures(ctx, "t", "f", transferstore.FailureFailed); n != 1 {
		t.Errorf("failed failures = %d, want 1 (must terminate, not loop)", n)
	}
	if n, _ := store.CountFailures(ctx, "t", "f", transferstore.FailurePending); n != 0 {
		t.Errorf("pending failures = %d, want 0", n)
	}
	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerificationFailed {
		t.Errorf("file state = %q, want verification_failed", got.VerificationState)
	}
}

// TestProcessDueFailures_TransientRepostErrorConsumesAttempt ensures that a
// persistent (but non-ErrNotExist) repost error is bounded by MaxReposts
// instead of retrying forever without advancing any counter.
func TestProcessDueFailures_TransientRepostErrorConsumesAttempt(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 2)

	stater := newFakeStater(mid(1))
	reposter := &fakeReposter{failWith: errors.New("upload pool unavailable")}

	cfg := Config{
		MaxReposts:        2,
		PropagationDelay:  time.Millisecond,
		DeferredBackoff:   time.Millisecond,
		MaxDeferredChecks: 1,
	}
	svc := New(store, stater, reposter, cfg, "w")

	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}

	// Passes 1-2 consume repost attempts; pass 3 STAT-rechecks and exhausts.
	now := time.Now().Add(time.Second)
	for i := 0; i < cfg.MaxReposts+1; i++ {
		if _, err := svc.ProcessDueFailures(ctx, now); err != nil {
			t.Fatalf("ProcessDueFailures #%d: %v", i+1, err)
		}
		now = now.Add(time.Hour)
	}

	if n, _ := store.CountFailures(ctx, "t", "f", transferstore.FailureFailed); n != 1 {
		t.Errorf("failed failures = %d, want 1 (must terminate, not loop)", n)
	}
}

func TestProcessDueFailures_ExhaustsAndMarksFailed(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 2)

	// m0 permanently missing; no reposts allowed -> straight to deferred checks.
	stater := newFakeStater(mid(0))
	cfg := Config{
		MaxReposts:        0,
		PropagationDelay:  time.Millisecond,
		DeferredBackoff:   time.Millisecond,
		MaxBackoff:        time.Millisecond,
		MaxDeferredChecks: 2,
	}
	svc := New(store, stater, &fakeReposter{}, cfg, "w")

	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}

	// Run enough cycles to exhaust deferred checks.
	now := time.Now()
	for i := 0; i < 5; i++ {
		now = now.Add(time.Hour)
		if _, err := svc.ProcessDueFailures(ctx, now); err != nil {
			t.Fatalf("ProcessDueFailures #%d: %v", i, err)
		}
	}

	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerificationFailed {
		t.Errorf("state = %q, want verification_failed", got.VerificationState)
	}
	if n, _ := store.CountFailures(ctx, "t", "f", transferstore.FailureFailed); n != 1 {
		t.Errorf("failed failures = %d, want 1", n)
	}
}

func TestProcessDueFailures_NothingDue(t *testing.T) {
	store := newTestStore(t)
	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	n, err := svc.ProcessDueFailures(context.Background(), time.Now())
	if err != nil {
		t.Fatalf("ProcessDueFailures: %v", err)
	}
	if n != 0 {
		t.Errorf("processed = %d, want 0", n)
	}
}

func TestVerifyDueFiles_VerifiesOnlyDueUploadedFiles(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()
	now := time.Now().UTC()

	writeManifest(t, store, "t", "due", 3)
	writeManifest(t, store, "t", "notdue", 3)
	if err := store.MarkUploaded(ctx, "t", "due", now.Add(-time.Hour), now.Add(-time.Minute)); err != nil {
		t.Fatal(err)
	}
	if err := store.MarkUploaded(ctx, "t", "notdue", now.Add(-time.Hour), now.Add(time.Hour)); err != nil {
		t.Fatal(err)
	}

	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	n, err := svc.VerifyDueFiles(ctx, now, 10)
	if err != nil {
		t.Fatalf("VerifyDueFiles: %v", err)
	}
	if n != 1 {
		t.Errorf("verified %d files, want 1 (only the due one)", n)
	}

	dueFile, _ := store.GetFile(ctx, "t", "due")
	if dueFile.VerificationState != transferstore.StateVerified {
		t.Errorf("due file state = %q, want verified", dueFile.VerificationState)
	}
	notDue, _ := store.GetFile(ctx, "t", "notdue")
	if notDue.VerificationState != transferstore.StateUploaded {
		t.Errorf("not-due file state = %q, want still uploaded", notDue.VerificationState)
	}
}

// fakeCleaner records which transfers cleanup was attempted for.
type fakeCleaner struct{ called []string }

func (c *fakeCleaner) CleanupTransfer(_ context.Context, transferID string) (bool, error) {
	c.called = append(c.called, transferID)
	return true, nil
}

func TestVerifyFile_TriggersCleanupWhenVerified(t *testing.T) {
	store, db := newTestStoreWithDB(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 3)
	insertCompletedItem(t, store, db, "t", "ci-cleanup")

	cleaner := &fakeCleaner{}
	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	svc.SetCleaner(cleaner)

	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}
	if len(cleaner.called) != 1 || cleaner.called[0] != "t" {
		t.Errorf("cleanup called for %v, want [t]", cleaner.called)
	}
}

func TestVerifyFile_NoCleanupWhenMissesRemain(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 3)

	cleaner := &fakeCleaner{}
	svc := New(store, newFakeStater(mid(1)), &fakeReposter{}, Config{}, "w")
	svc.SetCleaner(cleaner)

	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}
	if len(cleaner.called) != 0 {
		t.Errorf("cleanup must not run while misses remain, got %v", cleaner.called)
	}
}

func TestVerifyFile_UpdatesCompletedItemStatusOnVerified(t *testing.T) {
	store, db := newTestStoreWithDB(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 2)
	// Link the transfer to a completed item that starts pending_verification.
	if err := store.SetCompletedItemForTransfer(ctx, "t", "ci-1"); err != nil {
		t.Fatal(err)
	}
	if _, err := db.ExecContext(ctx, `INSERT INTO completed_items
		(id, path, size, nzb_path, created_at, job_data, verification_status)
		VALUES (?,?,?,?,?,?,?)`,
		"ci-1", "/d/a", 1, "/o/a.nzb", "2026-01-01T00:00:00Z", []byte("{}"), "pending_verification"); err != nil {
		t.Fatal(err)
	}

	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}

	var status string
	if err := db.QueryRowContext(ctx, "SELECT verification_status FROM completed_items WHERE id=?", "ci-1").Scan(&status); err != nil {
		t.Fatal(err)
	}
	if status != "verified" {
		t.Errorf("completed item status = %q, want verified", status)
	}
}

// insertCompletedItem creates a completed_items row in pending_verification and
// links it to the transfer, mirroring what the processor does at job completion.
func insertCompletedItem(t *testing.T, store *transferstore.Store, db *sql.DB, transferID, itemID string) {
	t.Helper()
	ctx := context.Background()
	if err := store.SetCompletedItemForTransfer(ctx, transferID, itemID); err != nil {
		t.Fatal(err)
	}
	if _, err := db.ExecContext(ctx, `INSERT INTO completed_items
		(id, path, size, nzb_path, created_at, job_data, verification_status)
		VALUES (?,?,?,?,?,?,?)`,
		itemID, "/d/a", 1, "/o/a.nzb", "2026-01-01T00:00:00Z", []byte("{}"), "pending_verification"); err != nil {
		t.Fatal(err)
	}
}

func completedItemStatus(t *testing.T, db *sql.DB, itemID string) string {
	t.Helper()
	var status string
	if err := db.QueryRowContext(context.Background(),
		"SELECT verification_status FROM completed_items WHERE id=?", itemID).Scan(&status); err != nil {
		t.Fatal(err)
	}
	return status
}

func TestVerifyFile_MissingManifestFailsTerminally(t *testing.T) {
	store, db := newTestStoreWithDB(t)
	ctx := context.Background()

	// transfer_files row pointing at a manifest that no longer exists.
	if err := store.UpsertFile(ctx, transferstore.TransferFile{
		TransferID:        "t",
		FileID:            "f",
		ManifestPath:      filepath.Join(t.TempDir(), "gone.manifest.zst"),
		SourcePath:        "/data/x.bin",
		FileRole:          "original",
		ArticleCount:      3,
		UploadState:       transferstore.StateUploaded,
		VerificationState: transferstore.StateUploaded,
	}); err != nil {
		t.Fatal(err)
	}
	insertCompletedItem(t, store, db, "t", "ci-1")

	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile with missing manifest must handle terminally, got err: %v", err)
	}

	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerificationFailed {
		t.Errorf("state = %q, want verification_failed", got.VerificationState)
	}
	if status := completedItemStatus(t, db, "ci-1"); status != "verification_failed" {
		t.Errorf("completed item status = %q, want verification_failed", status)
	}
}

func TestVerifyFile_CorruptManifestDefersWithBackoff(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()

	// A manifest file that exists but is garbage (transient/unknown error path).
	badPath := filepath.Join(t.TempDir(), "bad.manifest.zst")
	if err := os.WriteFile(badPath, []byte("not a manifest"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := store.UpsertFile(ctx, transferstore.TransferFile{
		TransferID:        "t",
		FileID:            "f",
		ManifestPath:      badPath,
		SourcePath:        "/data/x.bin",
		FileRole:          "original",
		ArticleCount:      3,
		UploadState:       transferstore.StateUploaded,
		VerificationState: transferstore.StateUploaded,
	}); err != nil {
		t.Fatal(err)
	}

	cfg := Config{DeferredBackoff: time.Minute, MaxDeferredChecks: 3}
	svc := New(store, newFakeStater(), &fakeReposter{}, cfg, "w")
	tf, _ := store.GetFile(ctx, "t", "f")
	before := time.Now()
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}

	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateUploaded {
		t.Errorf("state = %q, want uploaded (deferred)", got.VerificationState)
	}
	if got.CheckAttempts != 1 {
		t.Errorf("check_attempts = %d, want 1", got.CheckAttempts)
	}
	if got.NextCheckAt == nil || got.NextCheckAt.Before(before.Add(30*time.Second)) {
		t.Errorf("next_check_at = %v, want pushed out by backoff", got.NextCheckAt)
	}
	if got.LastError == "" {
		t.Error("last_error must record the manifest error")
	}
}

func TestVerifyFile_TransientErrorExhaustsToFailed(t *testing.T) {
	store, db := newTestStoreWithDB(t)
	ctx := context.Background()

	badPath := filepath.Join(t.TempDir(), "bad.manifest.zst")
	if err := os.WriteFile(badPath, []byte("still not a manifest"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := store.UpsertFile(ctx, transferstore.TransferFile{
		TransferID:        "t",
		FileID:            "f",
		ManifestPath:      badPath,
		SourcePath:        "/data/x.bin",
		FileRole:          "original",
		ArticleCount:      3,
		UploadState:       transferstore.StateUploaded,
		VerificationState: transferstore.StateUploaded,
	}); err != nil {
		t.Fatal(err)
	}
	insertCompletedItem(t, store, db, "t", "ci-1")

	cfg := Config{DeferredBackoff: time.Millisecond, MaxDeferredChecks: 2}
	svc := New(store, newFakeStater(), &fakeReposter{}, cfg, "w")

	for i := 0; i < 2; i++ {
		tf, _ := store.GetFile(ctx, "t", "f")
		if err := svc.VerifyFile(ctx, tf); err != nil {
			t.Fatalf("VerifyFile #%d: %v", i+1, err)
		}
	}

	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerificationFailed {
		t.Errorf("state = %q, want verification_failed after exhausting checks", got.VerificationState)
	}
	if status := completedItemStatus(t, db, "ci-1"); status != "verification_failed" {
		t.Errorf("completed item status = %q, want verification_failed", status)
	}
}

func TestRunOnce_DefersWhileUploadsSaturated(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 2)
	now := time.Now().Add(-time.Minute)
	if err := store.MarkUploaded(ctx, "t", "f", now, now); err != nil {
		t.Fatal(err)
	}

	busy := true
	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	svc.SetBusyCheck(func() bool { return busy })

	// While uploads saturate the shared pool, the sweep is skipped.
	svc.runOnce(ctx)
	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateUploaded {
		t.Fatalf("state = %q, want uploaded (deferred while busy)", got.VerificationState)
	}

	// Once the pool has spare capacity the file verifies normally.
	busy = false
	svc.runOnce(ctx)
	got, _ = store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerified {
		t.Fatalf("state = %q, want verified after busy clears", got.VerificationState)
	}
}

// insertBareCompletedItem creates a pending_verification completed item WITHOUT
// linking any transfer_files (an orphan, as left behind by pre-fix crashes or
// pre-durable upgrades).
func insertBareCompletedItem(t *testing.T, db *sql.DB, itemID string) {
	t.Helper()
	if _, err := db.ExecContext(context.Background(), `INSERT INTO completed_items
		(id, path, size, nzb_path, created_at, job_data, verification_status)
		VALUES (?,?,?,?,?,?,?)`,
		itemID, "/d/a", 1, "/o/a.nzb", "2026-01-01T00:00:00Z", []byte("{}"), "pending_verification"); err != nil {
		t.Fatal(err)
	}
}

func TestReconcileStuck_OrphanItemMarkedVerified(t *testing.T) {
	store, db := newTestStoreWithDB(t)
	ctx := context.Background()
	insertBareCompletedItem(t, db, "ci-orphan")

	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	if err := svc.ReconcileStuck(ctx); err != nil {
		t.Fatalf("ReconcileStuck: %v", err)
	}

	if status := completedItemStatus(t, db, "ci-orphan"); status != "verified" {
		t.Errorf("orphan item status = %q, want verified", status)
	}
}

func TestReconcileStuck_LegacyPendingFailureLeftAlone(t *testing.T) {
	store, db := newTestStoreWithDB(t)
	ctx := context.Background()
	insertBareCompletedItem(t, db, "ci-legacy")

	// Legacy STAT-only failure still being worked (transfer_id = item id).
	if err := store.AddFailure(ctx, transferstore.VerificationFailure{
		TransferID:    "ci-legacy",
		FileID:        transferstore.LegacyFileID,
		ArticleIndex:  -1,
		MessageID:     "<legacy>",
		NextAttemptAt: time.Now().Add(time.Hour),
	}); err != nil {
		t.Fatal(err)
	}

	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	if err := svc.ReconcileStuck(ctx); err != nil {
		t.Fatalf("ReconcileStuck: %v", err)
	}

	if status := completedItemStatus(t, db, "ci-legacy"); status != "pending_verification" {
		t.Errorf("item with pending legacy failure = %q, want pending_verification", status)
	}
}

func TestReconcileStuck_LegacyFailedFailureMarksFailed(t *testing.T) {
	store, db := newTestStoreWithDB(t)
	ctx := context.Background()
	insertBareCompletedItem(t, db, "ci-legacy")

	if err := store.AddFailure(ctx, transferstore.VerificationFailure{
		TransferID:    "ci-legacy",
		FileID:        transferstore.LegacyFileID,
		ArticleIndex:  -1,
		MessageID:     "<legacy>",
		State:         transferstore.FailureFailed,
		NextAttemptAt: time.Now(),
	}); err != nil {
		t.Fatal(err)
	}

	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	if err := svc.ReconcileStuck(ctx); err != nil {
		t.Fatalf("ReconcileStuck: %v", err)
	}

	if status := completedItemStatus(t, db, "ci-legacy"); status != "verification_failed" {
		t.Errorf("item with failed legacy failure = %q, want verification_failed", status)
	}
}

func TestReconcileStuck_TerminalFilesFinalized(t *testing.T) {
	store, db := newTestStoreWithDB(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 2)
	insertCompletedItem(t, store, db, "t", "ci-1")

	// The file already reached a terminal state but the item was never
	// finalized (pre-fix bug).
	if err := store.SetVerificationState(ctx, "t", "f", transferstore.StateVerified, nil, ""); err != nil {
		t.Fatal(err)
	}

	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	if err := svc.ReconcileStuck(ctx); err != nil {
		t.Fatalf("ReconcileStuck: %v", err)
	}

	if status := completedItemStatus(t, db, "ci-1"); status != "verified" {
		t.Errorf("item with all-verified files = %q, want verified", status)
	}
}

func TestReconcileStuck_ActiveVerificationLeftAlone(t *testing.T) {
	store, db := newTestStoreWithDB(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 2)
	insertCompletedItem(t, store, db, "t", "ci-1")

	now := time.Now()
	if err := store.MarkUploaded(ctx, "t", "f", now, now); err != nil {
		t.Fatal(err)
	}

	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	if err := svc.ReconcileStuck(ctx); err != nil {
		t.Fatalf("ReconcileStuck: %v", err)
	}

	if status := completedItemStatus(t, db, "ci-1"); status != "pending_verification" {
		t.Errorf("item with due file = %q, want pending_verification (service will verify it)", status)
	}
}

func TestProcessDueFailures_ExhaustedArticleFinalizesCompletedItem(t *testing.T) {
	store, db := newTestStoreWithDB(t)
	ctx := context.Background()
	writeManifest(t, store, "t", "f", 2)
	insertCompletedItem(t, store, db, "t", "ci-1")

	// Mark the file uploaded so it belongs to the verification pipeline, and
	// seed a failure that is already at the recheck stage.
	now := time.Now()
	if err := store.MarkUploaded(ctx, "t", "f", now, now); err != nil {
		t.Fatal(err)
	}
	if err := store.SetVerificationState(ctx, "t", "f", transferstore.StateVerifying, nil, ""); err != nil {
		t.Fatal(err)
	}
	if err := store.AddFailure(ctx, transferstore.VerificationFailure{
		TransferID:    "t",
		FileID:        "f",
		ArticleIndex:  0,
		MessageID:     mid(0),
		NextAttemptAt: now,
	}); err != nil {
		t.Fatal(err)
	}

	// Article stays missing; no reposts allowed, single deferred check → exhausted.
	cfg := Config{MaxReposts: 0, MaxDeferredChecks: 1, DeferredBackoff: time.Millisecond}
	svc := New(store, newFakeStater(mid(0)), &fakeReposter{}, cfg, "w")

	if _, err := svc.ProcessDueFailures(ctx, now.Add(time.Second)); err != nil {
		t.Fatalf("ProcessDueFailures: %v", err)
	}

	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerificationFailed {
		t.Fatalf("file state = %q, want verification_failed", got.VerificationState)
	}
	if status := completedItemStatus(t, db, "ci-1"); status != "verification_failed" {
		t.Errorf("completed item status = %q, want verification_failed (stuck pending_verification bug)", status)
	}
}
