package transfercleaner

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/database"
	"github.com/kipsilabs/postie/internal/transferstore"
)

func newTestStore(t *testing.T) *transferstore.Store {
	t.Helper()
	ctx := context.Background()
	db, err := database.New(ctx, config.DatabaseConfig{
		DatabaseType: "sqlite",
		DatabasePath: filepath.Join(t.TempDir(), "test.db"),
	})
	if err != nil {
		t.Fatalf("database.New: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	if err := db.GetMigrationRunner().MigrateUp(); err != nil {
		t.Fatalf("MigrateUp: %v", err)
	}
	return transferstore.New(db.DB)
}

// makeFile creates a file with some content and returns its path.
func makeFile(t *testing.T, dir, name string) string {
	t.Helper()
	p := filepath.Join(dir, name)
	if err := os.WriteFile(p, []byte("data"), 0o644); err != nil {
		t.Fatalf("write %s: %v", p, err)
	}
	return p
}

func exists(p string) bool {
	_, err := os.Stat(p)
	return err == nil
}

// seedFile creates real source + manifest files and a transfer_files row.
func seedFile(t *testing.T, store *transferstore.Store, dir, transferID, fileID, role, state, policy string) (src, man string) {
	t.Helper()
	src = makeFile(t, dir, fileID+".src")
	man = makeFile(t, dir, fileID+".manifest")
	ctx := context.Background()
	if err := store.UpsertFile(ctx, transferstore.TransferFile{
		TransferID:        transferID,
		FileID:            fileID,
		ManifestPath:      man,
		SourcePath:        src,
		FileRole:          role,
		VerificationState: state,
		CleanupPolicy:     policy,
	}); err != nil {
		t.Fatalf("UpsertFile: %v", err)
	}
	return src, man
}

func TestCleanup_AllVerified_DeletesPerPolicy(t *testing.T) {
	store := newTestStore(t)
	dir := t.TempDir()
	ctx := context.Background()

	origSrc, origMan := seedFile(t, store, dir, "t", "orig", "original", transferstore.StateVerified, transferstore.CleanupDeleteOriginal)
	par2Src, par2Man := seedFile(t, store, dir, "t", "p2", "generated_par2", transferstore.StateVerified, "")

	c := New(store, false, nil) // maintainPar2=false
	done, err := c.CleanupTransfer(ctx, "t")
	if err != nil {
		t.Fatalf("CleanupTransfer: %v", err)
	}
	if !done {
		t.Fatal("expected cleanup to run (all verified)")
	}

	if exists(origSrc) {
		t.Error("original source should be deleted (delete_original policy)")
	}
	if exists(par2Src) {
		t.Error("generated par2 should be deleted (maintain_par2=false)")
	}
	if exists(origMan) || exists(par2Man) {
		t.Error("manifests should be deleted after cleanup")
	}
	// Rows removed after cleanup.
	files, _ := store.ListFilesByTransfer(ctx, "t")
	if len(files) != 0 {
		t.Errorf("transfer_files rows = %d, want 0 after cleanup", len(files))
	}
}

func TestCleanup_MaintainPar2_RetainsPar2(t *testing.T) {
	store := newTestStore(t)
	dir := t.TempDir()
	par2Src, _ := seedFile(t, store, dir, "t", "p2", "generated_par2", transferstore.StateVerified, "")

	c := New(store, true, nil) // maintainPar2=true
	if _, err := c.CleanupTransfer(context.Background(), "t"); err != nil {
		t.Fatal(err)
	}
	if !exists(par2Src) {
		t.Error("par2 should be retained when maintain_par2=true")
	}
}

func TestCleanup_NoDeletePolicy_RetainsOriginal(t *testing.T) {
	store := newTestStore(t)
	dir := t.TempDir()
	origSrc, origMan := seedFile(t, store, dir, "t", "orig", "original", transferstore.StateVerified, "")

	c := New(store, false, nil)
	if _, err := c.CleanupTransfer(context.Background(), "t"); err != nil {
		t.Fatal(err)
	}
	if !exists(origSrc) {
		t.Error("original should be retained when no delete_original policy")
	}
	if exists(origMan) {
		t.Error("manifest should still be cleaned after verification")
	}
}

func TestCleanup_AnyFailed_RetainsEverything(t *testing.T) {
	store := newTestStore(t)
	dir := t.TempDir()
	ctx := context.Background()

	origSrc, origMan := seedFile(t, store, dir, "t", "orig", "original", transferstore.StateVerified, transferstore.CleanupDeleteOriginal)
	failSrc, failMan := seedFile(t, store, dir, "t", "f", "original", transferstore.StateVerificationFailed, transferstore.CleanupDeleteOriginal)

	c := New(store, false, nil)
	done, err := c.CleanupTransfer(ctx, "t")
	if err != nil {
		t.Fatalf("CleanupTransfer: %v", err)
	}
	if done {
		t.Error("cleanup must not complete when a file failed verification")
	}
	for _, p := range []string{origSrc, origMan, failSrc, failMan} {
		if !exists(p) {
			t.Errorf("file %s must be retained when verification failed for the transfer", p)
		}
	}
	if files, _ := store.ListFilesByTransfer(ctx, "t"); len(files) != 2 {
		t.Errorf("rows must be retained on failure, got %d", len(files))
	}
}

func TestCleanup_NotAllVerified_NoOp(t *testing.T) {
	store := newTestStore(t)
	dir := t.TempDir()
	ctx := context.Background()

	origSrc, _ := seedFile(t, store, dir, "t", "orig", "original", transferstore.StateVerified, transferstore.CleanupDeleteOriginal)
	seedFile(t, store, dir, "t", "v", "original", transferstore.StateVerifying, "")

	c := New(store, false, nil)
	done, err := c.CleanupTransfer(ctx, "t")
	if err != nil {
		t.Fatalf("CleanupTransfer: %v", err)
	}
	if done {
		t.Error("cleanup must not run while a file is still verifying")
	}
	if !exists(origSrc) {
		t.Error("nothing should be deleted while verification is in progress")
	}
}

func TestCleanup_RunsScriptOnceWhenAllVerified(t *testing.T) {
	store := newTestStore(t)
	dir := t.TempDir()
	ctx := context.Background()
	seedFile(t, store, dir, "t", "f1", "original", transferstore.StateVerified, "")
	seedFile(t, store, dir, "t", "f2", "original", transferstore.StateVerified, "")

	var calls []string
	runScript := func(_ context.Context, transferID string, files []transferstore.TransferFile) error {
		calls = append(calls, transferID)
		return nil
	}

	c := New(store, true, runScript)
	if _, err := c.CleanupTransfer(ctx, "t"); err != nil {
		t.Fatalf("CleanupTransfer: %v", err)
	}
	if len(calls) != 1 || calls[0] != "t" {
		t.Errorf("script runner called %v, want [t] (once)", calls)
	}
}

func TestCleanup_DoesNotRunScriptOnFailure(t *testing.T) {
	store := newTestStore(t)
	dir := t.TempDir()
	ctx := context.Background()
	seedFile(t, store, dir, "t", "ok", "original", transferstore.StateVerified, "")
	seedFile(t, store, dir, "t", "bad", "original", transferstore.StateVerificationFailed, "")

	called := false
	c := New(store, true, func(_ context.Context, _ string, _ []transferstore.TransferFile) error {
		called = true
		return nil
	})
	if _, err := c.CleanupTransfer(ctx, "t"); err != nil {
		t.Fatalf("CleanupTransfer: %v", err)
	}
	if called {
		t.Error("post-upload script must not run when verification failed")
	}
}
