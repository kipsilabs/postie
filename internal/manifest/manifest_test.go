package manifest

import (
	"errors"
	"io"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/kipsilabs/postie/internal/article"
)

func sampleRecords(n int) []ArticleRecord {
	recs := make([]ArticleRecord, n)
	for i := range recs {
		recs[i] = ArticleRecord{
			Index:           i,
			SourcePath:      "/data/show/ep1.mkv",
			FileRole:        RoleOriginal,
			Offset:          int64(i) * 768000,
			BodySize:        768000,
			MessageID:       "<part" + string(rune('a'+i)) + "@postie>",
			Subject:         "subject line " + string(rune('a'+i)),
			OriginalSubject: "ep1.mkv",
			From:            "Poster <poster@example.com>",
			Groups:          []string{"alt.binaries.test", "alt.binaries.misc"},
			Date:            time.Unix(1700000000+int64(i), 0).UTC(),
			CustomHeaders:   map[string]string{"X-Foo": "bar"},
			XNxgHeader:      "nxg-token",
			FileName:        "ep1.mkv",
			PartNumber:      i + 1,
			TotalParts:      n,
			FileSize:        int64(n) * 768000,
		}
	}
	return recs
}

func TestWriteReadRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "t1", "file1.jsonl.zst")
	want := sampleRecords(5)

	w, err := NewWriter(path)
	if err != nil {
		t.Fatalf("NewWriter: %v", err)
	}
	for _, rec := range want {
		if err := w.Write(rec); err != nil {
			t.Fatalf("Write: %v", err)
		}
	}
	if w.Count() != len(want) {
		t.Errorf("Count() = %d, want %d", w.Count(), len(want))
	}
	if err := w.Commit(); err != nil {
		t.Fatalf("Commit: %v", err)
	}

	r, err := OpenReader(path)
	if err != nil {
		t.Fatalf("OpenReader: %v", err)
	}
	defer func() { _ = r.Close() }()

	if r.Version() != Version {
		t.Errorf("Version() = %d, want %d", r.Version(), Version)
	}

	var got []ArticleRecord
	for {
		rec, err := r.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatalf("Next: %v", err)
		}
		got = append(got, rec)
	}

	if len(got) != len(want) {
		t.Fatalf("read %d records, want %d", len(got), len(want))
	}
	for i := range want {
		if got[i].MessageID != want[i].MessageID ||
			got[i].Offset != want[i].Offset ||
			got[i].BodySize != want[i].BodySize ||
			got[i].PartNumber != want[i].PartNumber ||
			got[i].Subject != want[i].Subject ||
			!got[i].Date.Equal(want[i].Date) ||
			got[i].XNxgHeader != want[i].XNxgHeader {
			t.Errorf("record %d mismatch:\n got %+v\nwant %+v", i, got[i], want[i])
		}
	}
}

func TestCommitIsAtomic(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "t1", "file1.jsonl.zst")

	w, err := NewWriter(path)
	if err != nil {
		t.Fatalf("NewWriter: %v", err)
	}
	if err := w.Write(sampleRecords(1)[0]); err != nil {
		t.Fatalf("Write: %v", err)
	}

	// Before commit, the final path must not exist; only the temp file does.
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Errorf("final manifest exists before Commit (err=%v)", err)
	}
	if _, err := os.Stat(path + ".tmp"); err != nil {
		t.Errorf("temp manifest missing before Commit: %v", err)
	}

	if err := w.Commit(); err != nil {
		t.Fatalf("Commit: %v", err)
	}

	if _, err := os.Stat(path); err != nil {
		t.Errorf("final manifest missing after Commit: %v", err)
	}
	if _, err := os.Stat(path + ".tmp"); !os.IsNotExist(err) {
		t.Errorf("temp manifest still exists after Commit (err=%v)", err)
	}
}

func TestAbortRemovesTempAndLeavesNoFinal(t *testing.T) {
	path := filepath.Join(t.TempDir(), "t1", "file1.jsonl.zst")
	w, err := NewWriter(path)
	if err != nil {
		t.Fatalf("NewWriter: %v", err)
	}
	_ = w.Write(sampleRecords(1)[0])
	if err := w.Abort(); err != nil {
		t.Fatalf("Abort: %v", err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Errorf("final manifest exists after Abort")
	}
	if _, err := os.Stat(path + ".tmp"); !os.IsNotExist(err) {
		t.Errorf("temp manifest exists after Abort")
	}
	// Abort after close is a no-op.
	if err := w.Abort(); err != nil {
		t.Errorf("second Abort = %v, want nil", err)
	}
}

func TestOpenReaderRejectsNonManifest(t *testing.T) {
	path := filepath.Join(t.TempDir(), "bogus.jsonl.zst")
	if err := os.WriteFile(path, []byte("not a manifest"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := OpenReader(path); err == nil {
		t.Error("OpenReader accepted a non-manifest file")
	}
}

func TestRecordFromArticle(t *testing.T) {
	a := &article.Article{
		MessageID:       "<x@postie>",
		Subject:         "subj",
		OriginalSubject: "orig",
		From:            "from",
		Groups:          []string{"g1"},
		PartNumber:      2,
		TotalParts:      10,
		FileName:        "f.bin",
		Date:            time.Unix(1700000000, 0).UTC(),
		Offset:          1536000,
		Size:            768000,
		FileSize:        7680000,
		CustomHeaders:   map[string]string{"H": "v"},
		XNxgHeader:      "nxg",
	}
	rec := RecordFromArticle(7, "/src/f.bin", RoleGeneratedPar2, a)

	if rec.Index != 7 || rec.SourcePath != "/src/f.bin" || rec.FileRole != RoleGeneratedPar2 {
		t.Errorf("identity fields wrong: %+v", rec)
	}
	if rec.MessageID != a.MessageID || rec.Offset != a.Offset || rec.BodySize != a.Size ||
		rec.PartNumber != a.PartNumber || rec.TotalParts != a.TotalParts || rec.XNxgHeader != a.XNxgHeader {
		t.Errorf("article fields not copied: %+v", rec)
	}
}

func TestFilePath(t *testing.T) {
	got := FilePath("/base", "tid-123", "fid-9")
	want := filepath.Join("/base", "tid-123", "fid-9.jsonl.zst")
	if got != want {
		t.Errorf("FilePath = %q, want %q", got, want)
	}
}

func TestLargeManifestStreams(t *testing.T) {
	// A larger manifest exercises multi-record streaming round-trips.
	path := filepath.Join(t.TempDir(), "big", "f.jsonl.zst")
	w, err := NewWriter(path)
	if err != nil {
		t.Fatal(err)
	}
	const n = 10000
	for i := 0; i < n; i++ {
		if err := w.Write(ArticleRecord{Index: i, MessageID: "<m>", Offset: int64(i), BodySize: 1}); err != nil {
			t.Fatalf("Write %d: %v", i, err)
		}
	}
	if err := w.Commit(); err != nil {
		t.Fatal(err)
	}

	r, err := OpenReader(path)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = r.Close() }()

	count := 0
	for {
		rec, err := r.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatalf("Next at %d: %v", count, err)
		}
		if rec.Index != count {
			t.Fatalf("record %d has Index %d", count, rec.Index)
		}
		count++
	}
	if count != n {
		t.Errorf("streamed %d records, want %d", count, n)
	}
}
