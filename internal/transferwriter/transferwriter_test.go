package transferwriter

import (
	"context"
	"path/filepath"
	"testing"
	"time"

	"github.com/kipsilabs/postie/internal/article"
	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/database"
	"github.com/kipsilabs/postie/internal/manifest"
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

func TestRecorder_WritesManifestAndRow(t *testing.T) {
	store := newTestStore(t)
	baseDir := t.TempDir()
	ctx := context.Background()

	rec := New("tid-1", baseDir, store)

	articles := []*article.Article{
		{MessageID: "a@p", Subject: "s0", Offset: 0, Size: 10, PartNumber: 1, TotalParts: 2, FileName: "movie.mkv"},
		{MessageID: "b@p", Subject: "s1", Offset: 10, Size: 10, PartNumber: 2, TotalParts: 2, FileName: "movie.mkv"},
	}
	if err := rec.RecordFile(ctx, "/data/movie.mkv", articles); err != nil {
		t.Fatalf("RecordFile: %v", err)
	}

	files, err := store.ListFilesByTransfer(ctx, "tid-1")
	if err != nil {
		t.Fatalf("ListFilesByTransfer: %v", err)
	}
	if len(files) != 1 {
		t.Fatalf("transfer_files rows = %d, want 1", len(files))
	}
	tf := files[0]
	if tf.SourcePath != "/data/movie.mkv" {
		t.Errorf("SourcePath = %q, want /data/movie.mkv", tf.SourcePath)
	}
	if tf.FileRole != string(manifest.RoleOriginal) {
		t.Errorf("FileRole = %q, want original", tf.FileRole)
	}
	if tf.ArticleCount != 2 {
		t.Errorf("ArticleCount = %d, want 2", tf.ArticleCount)
	}
	if tf.UploadState != transferstore.StatePlanned {
		t.Errorf("UploadState = %q, want planned", tf.UploadState)
	}

	// Manifest must exist at the recorded path and stream both articles back.
	r, err := manifest.OpenReader(tf.ManifestPath)
	if err != nil {
		t.Fatalf("OpenReader(%q): %v", tf.ManifestPath, err)
	}
	defer func() { _ = r.Close() }()

	var ids []string
	for {
		mr, err := r.Next()
		if err != nil {
			break
		}
		ids = append(ids, mr.MessageID)
	}
	if len(ids) != 2 || ids[0] != "a@p" || ids[1] != "b@p" {
		t.Errorf("manifest message ids = %v, want [a@p b@p]", ids)
	}
}

func TestRecorder_InfersPar2Role(t *testing.T) {
	store := newTestStore(t)
	rec := New("tid-2", t.TempDir(), store)
	ctx := context.Background()

	if err := rec.RecordFile(ctx, "/data/movie.vol00+1.par2", []*article.Article{
		{MessageID: "x@p", Offset: 0, Size: 5},
	}); err != nil {
		t.Fatalf("RecordFile: %v", err)
	}

	files, _ := store.ListFilesByTransfer(ctx, "tid-2")
	if len(files) != 1 || files[0].FileRole != string(manifest.RoleGeneratedPar2) {
		t.Errorf("expected generated_par2 role, got %+v", files)
	}
}

func TestRecorder_DeterministicFileIDUpdatesInPlace(t *testing.T) {
	store := newTestStore(t)
	rec := New("tid-3", t.TempDir(), store)
	ctx := context.Background()

	arts := []*article.Article{{MessageID: "a", Offset: 0, Size: 1}}
	if err := rec.RecordFile(ctx, "/data/a.bin", arts); err != nil {
		t.Fatal(err)
	}
	// Re-recording the same source path must update the same row, not add one.
	if err := rec.RecordFile(ctx, "/data/a.bin", arts); err != nil {
		t.Fatal(err)
	}
	files, _ := store.ListFilesByTransfer(ctx, "tid-3")
	if len(files) != 1 {
		t.Errorf("re-record created %d rows, want 1 (stable file id)", len(files))
	}
}

func TestRecorder_CompleteUploadMarksFilesUploaded(t *testing.T) {
	store := newTestStore(t)
	rec := New("tid-up", t.TempDir(), store)
	ctx := context.Background()

	if err := rec.RecordFile(ctx, "/data/a.mkv", []*article.Article{{MessageID: "a", Offset: 0, Size: 1}}); err != nil {
		t.Fatal(err)
	}
	if err := rec.RecordFile(ctx, "/data/a.par2", []*article.Article{{MessageID: "p", Offset: 0, Size: 1}}); err != nil {
		t.Fatal(err)
	}

	posted := time.Unix(1700000000, 0).UTC()
	nextCheck := posted.Add(10 * time.Second)
	if err := rec.CompleteUpload(ctx, posted, nextCheck, true); err != nil {
		t.Fatalf("CompleteUpload: %v", err)
	}

	files, _ := store.ListFilesByTransfer(ctx, "tid-up")
	if len(files) != 2 {
		t.Fatalf("files = %d, want 2", len(files))
	}
	for _, f := range files {
		if f.UploadState != transferstore.StateUploaded || f.VerificationState != transferstore.StateUploaded {
			t.Errorf("file %s states = %q/%q, want uploaded/uploaded", f.FileID, f.UploadState, f.VerificationState)
		}
		if f.NextCheckAt == nil || !f.NextCheckAt.Equal(nextCheck) {
			t.Errorf("file %s NextCheckAt = %v, want %v", f.FileID, f.NextCheckAt, nextCheck)
		}
		// delete_original requested: the original file carries the policy, the
		// par2 file does not (par2 cleanup is governed by maintain_par2 config).
		if f.FileRole == string(manifest.RoleOriginal) && f.CleanupPolicy != transferstore.CleanupDeleteOriginal {
			t.Errorf("original file cleanup policy = %q, want %q", f.CleanupPolicy, transferstore.CleanupDeleteOriginal)
		}
		if f.FileRole == string(manifest.RoleGeneratedPar2) && f.CleanupPolicy == transferstore.CleanupDeleteOriginal {
			t.Errorf("par2 file should not carry delete_original policy")
		}
	}
}

func TestRecorder_ExistingArticles(t *testing.T) {
	store := newTestStore(t)
	dir := t.TempDir()
	rec := New("tid-rec", dir, store)
	ctx := context.Background()

	// No manifest yet -> not found.
	if _, ok, err := rec.ExistingArticles(ctx, "/data/a.mkv"); err != nil || ok {
		t.Fatalf("ExistingArticles before record: ok=%v err=%v, want false,nil", ok, err)
	}

	arts := []*article.Article{
		{MessageID: "a@p", Offset: 0, Size: 10, PartNumber: 1, TotalParts: 2, Subject: "s0"},
		{MessageID: "b@p", Offset: 10, Size: 10, PartNumber: 2, TotalParts: 2, Subject: "s1"},
	}
	if err := rec.RecordFile(ctx, "/data/a.mkv", arts); err != nil {
		t.Fatal(err)
	}

	recs, ok, err := rec.ExistingArticles(ctx, "/data/a.mkv")
	if err != nil || !ok {
		t.Fatalf("ExistingArticles after record: ok=%v err=%v, want true,nil", ok, err)
	}
	if len(recs) != 2 || recs[0].MessageID != "a@p" || recs[1].MessageID != "b@p" {
		t.Errorf("records = %+v, want the 2 recorded message ids in order", recs)
	}
	if recs[1].Offset != 10 || recs[1].BodySize != 10 {
		t.Errorf("record offsets/size not preserved: %+v", recs[1])
	}
}
