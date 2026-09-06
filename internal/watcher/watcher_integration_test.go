package watcher

// Integration tests for the watcher's single-NZB-per-folder logic, covering bugs
// reported in github.com/kipsilabs/postie/issues/168.
//
// Run with: go test ./internal/watcher/... -run "Integration" -v -timeout 60s

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/kipsilabs/postie/internal/config"
)

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// createIntegrationWatcher builds a Watcher configured for integration tests:
// - MinFileSize=10 bytes
// - MinFileAge="2s" so files whose modtime is old enough are treated as stable
// - SizeThreshold=0 (no threshold filter in scanDirectoryGroupByFolder)
func createIntegrationWatcher(t *testing.T, watchDir string) (*Watcher, *mockQueueWithDuplicateCheck) {
	t.Helper()
	cfg := config.WatcherConfig{
		Enabled:            true,
		WatchDirectory:     watchDir,
		SizeThreshold:      0, // no threshold — not checked by scanDirectoryGroupByFolder directly
		MinFileSize:        10,
		CheckInterval:      config.Duration("1s"),
		SingleNzbPerFolder: true,
		MinFileAge:         config.Duration("2s"),
		IgnorePatterns:     []string{},
	}
	q := &mockQueueWithDuplicateCheck{addFileCalls: make([]string, 0)}
	w := New(cfg, q, nil, watchDir)
	return w, q
}

// createStableFile creates a file whose modification time is in the past so that
// isFileStable() will pass the age check.  It also primes the watcher's size
// cache (calling isFileSizeStable twice — first to insert, second to verify
// stability), so the file passes the size-stability check on the next real scan.
func createStableFile(t *testing.T, w *Watcher, path string, content []byte) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		t.Fatalf("createStableFile mkdir: %v", err)
	}
	if err := os.WriteFile(path, content, 0644); err != nil {
		t.Fatalf("createStableFile write: %v", err)
	}
	// Back-date modification time so MinFileAge passes.
	past := time.Now().Add(-5 * time.Minute)
	if err := os.Chtimes(path, past, past); err != nil {
		t.Fatalf("createStableFile chtimes: %v", err)
	}
	// Prime size cache: first call records the size, second call confirms stability.
	w.isFileSizeStable(path, int64(len(content)))
	w.isFileSizeStable(path, int64(len(content)))
}

// queuedFolderPaths returns paths from the mock queue that start with "FOLDER:".
func queuedFolderPaths(q *mockQueueWithDuplicateCheck) []string {
	q.mu.Lock()
	defer q.mu.Unlock()
	var out []string
	for _, p := range q.addFileCalls {
		if strings.HasPrefix(p, "FOLDER:") {
			out = append(out, p)
		}
	}
	return out
}

// queuedIndividualPaths returns non-FOLDER paths from the mock queue.
func queuedIndividualPaths(q *mockQueueWithDuplicateCheck) []string {
	q.mu.Lock()
	defer q.mu.Unlock()
	var out []string
	for _, p := range q.addFileCalls {
		if !strings.HasPrefix(p, "FOLDER:") {
			out = append(out, p)
		}
	}
	return out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// TestIntegration_ScanDirectoryGroupByFolder_FlatDirectory verifies that a single
// directory containing files (no nested subdirectories) produces exactly one
// FOLDER: queue entry whose path refers to that directory.
func TestIntegration_ScanDirectoryGroupByFolder_FlatDirectory(t *testing.T) {
	watchDir := t.TempDir()
	w, q := createIntegrationWatcher(t, watchDir)

	// watch/MyShow/
	//   ep01.rar   (100 bytes)
	//   ep02.rar   (100 bytes)
	showDir := filepath.Join(watchDir, "MyShow")
	createStableFile(t, w, filepath.Join(showDir, "ep01.rar"), make([]byte, 100))
	createStableFile(t, w, filepath.Join(showDir, "ep02.rar"), make([]byte, 100))

	if err := w.scanDirectoryGroupByFolder(context.Background()); err != nil {
		t.Fatalf("scanDirectoryGroupByFolder: %v", err)
	}

	folders := queuedFolderPaths(q)
	if len(folders) != 1 {
		t.Fatalf("expected 1 FOLDER queue entry, got %d: %v", len(folders), folders)
	}

	expectedPath := "FOLDER:" + showDir
	if folders[0] != expectedPath {
		t.Errorf("expected queue path %q, got %q", expectedPath, folders[0])
	}

	// No individual (non-FOLDER) paths.
	if ind := queuedIndividualPaths(q); len(ind) != 0 {
		t.Errorf("expected no individual paths, got: %v", ind)
	}
}

// TestIntegration_ScanDirectoryGroupByFolder_DirectoryWithSubdirs verifies that a
// directory containing files AND nested subdirectories is treated as a single unit —
// exactly ONE FOLDER: entry for the top-level directory, NOT separate entries for
// the subdirectories.
// Regression test for the "main folder NZB missing" bug in issue #168.
func TestIntegration_ScanDirectoryGroupByFolder_DirectoryWithSubdirs(t *testing.T) {
	watchDir := t.TempDir()
	w, q := createIntegrationWatcher(t, watchDir)

	// watch/SunMoments/
	//   01.rar         (root files)
	//   Proof/
	//     proof.jpg
	//   Sample/
	//     sample.mkv
	base := filepath.Join(watchDir, "SunMoments")
	createStableFile(t, w, filepath.Join(base, "01.rar"), make([]byte, 200))
	createStableFile(t, w, filepath.Join(base, "Proof", "proof.jpg"), make([]byte, 150))
	createStableFile(t, w, filepath.Join(base, "Sample", "sample.mkv"), make([]byte, 300))

	if err := w.scanDirectoryGroupByFolder(context.Background()); err != nil {
		t.Fatalf("scanDirectoryGroupByFolder: %v", err)
	}

	folders := queuedFolderPaths(q)

	// Must be exactly ONE entry — not three (one per rar/jpg/mkv directory).
	if len(folders) != 1 {
		t.Fatalf("expected 1 FOLDER queue entry, got %d: %v", len(folders), folders)
	}

	expectedPath := "FOLDER:" + base
	if folders[0] != expectedPath {
		t.Errorf("expected FOLDER path %q, got %q", expectedPath, folders[0])
	}

	// The path must point to SunMoments, NOT to the watch folder.
	watchFolderEntry := "FOLDER:" + watchDir
	if folders[0] == watchFolderEntry {
		t.Errorf("NZB named after watch folder (%q) — should be named after the sub-directory", watchDir)
	}
}

// TestIntegration_ScanDirectoryGroupByFolder_MultipleTopLevelDirs verifies that
// when the watch folder contains two distinct top-level directories, each gets its
// own FOLDER: queue entry with the correct path.
// Regression test for "NZB named after watch folder instead of subfolder" (#190).
func TestIntegration_ScanDirectoryGroupByFolder_MultipleTopLevelDirs(t *testing.T) {
	watchDir := t.TempDir()
	w, q := createIntegrationWatcher(t, watchDir)

	// watch/
	//   Dir1/a.rar
	//   Dir2/b.rar
	dir1 := filepath.Join(watchDir, "Dir1")
	dir2 := filepath.Join(watchDir, "Dir2")
	createStableFile(t, w, filepath.Join(dir1, "a.rar"), make([]byte, 100))
	createStableFile(t, w, filepath.Join(dir2, "b.rar"), make([]byte, 100))

	if err := w.scanDirectoryGroupByFolder(context.Background()); err != nil {
		t.Fatalf("scanDirectoryGroupByFolder: %v", err)
	}

	folders := queuedFolderPaths(q)
	if len(folders) != 2 {
		t.Fatalf("expected 2 FOLDER queue entries, got %d: %v", len(folders), folders)
	}

	hasDir1, hasDir2 := false, false
	for _, f := range folders {
		switch f {
		case "FOLDER:" + dir1:
			hasDir1 = true
		case "FOLDER:" + dir2:
			hasDir2 = true
		}
		// Neither must be the watch folder itself.
		if f == "FOLDER:"+watchDir {
			t.Errorf("queue contains watch folder path — should be named after subdirectory: %q", f)
		}
	}
	if !hasDir1 {
		t.Errorf("Dir1 not found in queue; entries: %v", folders)
	}
	if !hasDir2 {
		t.Errorf("Dir2 not found in queue; entries: %v", folders)
	}
}

// TestIntegration_ScanDirectoryGroupByFolder_NzbNameNotWatchFolder verifies that
// the folder name embedded in the queue path is the subdirectory name, not the
// watch folder name. This is how the processor derives the NZB filename.
// Regression test for github.com/kipsilabs/postie#190.
func TestIntegration_ScanDirectoryGroupByFolder_NzbNameNotWatchFolder(t *testing.T) {
	watchDir := t.TempDir()
	w, q := createIntegrationWatcher(t, watchDir)

	expectedDirName := "TheMovie2024"
	movieDir := filepath.Join(watchDir, expectedDirName)
	createStableFile(t, w, filepath.Join(movieDir, "part01.rar"), make([]byte, 100))
	createStableFile(t, w, filepath.Join(movieDir, "Extras", "extra.mkv"), make([]byte, 100))

	if err := w.scanDirectoryGroupByFolder(context.Background()); err != nil {
		t.Fatalf("scanDirectoryGroupByFolder: %v", err)
	}

	folders := queuedFolderPaths(q)
	if len(folders) != 1 {
		t.Fatalf("expected 1 FOLDER entry, got %d: %v", len(folders), folders)
	}

	// Strip the "FOLDER:" prefix and extract the base name — this becomes the NZB name.
	folderPath := strings.TrimPrefix(folders[0], "FOLDER:")
	derivedName := filepath.Base(folderPath)

	if derivedName != expectedDirName {
		t.Errorf("NZB name would be %q, expected %q — NZB would be named after watch folder", derivedName, expectedDirName)
	}
	if derivedName == filepath.Base(watchDir) {
		t.Errorf("NZB name equals watch folder base name (%q) — the #190 bug is present", derivedName)
	}
}

// TestIntegration_ScanDirectoryGroupByFolder_FilesDirectlyInWatchFolder verifies
// that files sitting directly in the watch folder (not in any subdirectory) are
// queued as individual entries, not as a FOLDER: job.
func TestIntegration_ScanDirectoryGroupByFolder_FilesDirectlyInWatchFolder(t *testing.T) {
	watchDir := t.TempDir()
	w, q := createIntegrationWatcher(t, watchDir)

	// One file directly in the watch folder, one in a subdirectory.
	createStableFile(t, w, filepath.Join(watchDir, "standalone.rar"), make([]byte, 100))
	subDir := filepath.Join(watchDir, "SubDir")
	createStableFile(t, w, filepath.Join(subDir, "grouped.rar"), make([]byte, 100))

	if err := w.scanDirectoryGroupByFolder(context.Background()); err != nil {
		t.Fatalf("scanDirectoryGroupByFolder: %v", err)
	}

	folders := queuedFolderPaths(q)
	individuals := queuedIndividualPaths(q)

	// SubDir → 1 FOLDER entry; standalone.rar → 1 individual entry.
	if len(folders) != 1 {
		t.Errorf("expected 1 FOLDER entry, got %d: %v", len(folders), folders)
	}
	if len(individuals) != 1 {
		t.Errorf("expected 1 individual entry, got %d: %v", len(individuals), individuals)
	}
	if len(individuals) > 0 && !strings.HasSuffix(individuals[0], "standalone.rar") {
		t.Errorf("expected standalone.rar as individual entry, got %q", individuals[0])
	}
}

// TestIntegration_ScanDirectoryGroupByFolder_DeduplicatesAlreadyQueuedFolders
// verifies that if a folder is already in the queue, it is not added again.
func TestIntegration_ScanDirectoryGroupByFolder_DeduplicatesAlreadyQueuedFolders(t *testing.T) {
	watchDir := t.TempDir()
	w, q := createIntegrationWatcher(t, watchDir)

	showDir := filepath.Join(watchDir, "ShowA")
	createStableFile(t, w, filepath.Join(showDir, "ep.rar"), make([]byte, 100))

	// First scan — should add the folder.
	if err := w.scanDirectoryGroupByFolder(context.Background()); err != nil {
		t.Fatalf("first scan: %v", err)
	}
	countAfterFirst := len(queuedFolderPaths(q))
	if countAfterFirst != 1 {
		t.Fatalf("expected 1 entry after first scan, got %d", countAfterFirst)
	}

	// Second scan — folder already in queue, must not add duplicate.
	if err := w.scanDirectoryGroupByFolder(context.Background()); err != nil {
		t.Fatalf("second scan: %v", err)
	}
	countAfterSecond := len(queuedFolderPaths(q))
	if countAfterSecond != 1 {
		t.Errorf("expected still 1 entry after second scan (dedup), got %d", countAfterSecond)
	}
}

// TestIntegration_ScanDirectoryGroupByFolder_EmptyDirectory verifies that an empty
// watch directory produces no queue entries and no error.
func TestIntegration_ScanDirectoryGroupByFolder_EmptyDirectory(t *testing.T) {
	watchDir := t.TempDir()
	w, q := createIntegrationWatcher(t, watchDir)

	if err := w.scanDirectoryGroupByFolder(context.Background()); err != nil {
		t.Fatalf("scanDirectoryGroupByFolder on empty dir: %v", err)
	}

	if all := q.addFileCalls; len(all) != 0 {
		t.Errorf("expected no queue entries for empty directory, got: %v", all)
	}
}

// TestIntegration_ScanDirectoryGroupByFolder_SkipsTooSmallFiles verifies that files
// smaller than MinFileSize are excluded from grouping.
func TestIntegration_ScanDirectoryGroupByFolder_SkipsTooSmallFiles(t *testing.T) {
	watchDir := t.TempDir()
	w, q := createIntegrationWatcher(t, watchDir) // MinFileSize=10

	showDir := filepath.Join(watchDir, "TinyFiles")
	// File smaller than MinFileSize (10 bytes) — should be excluded.
	createStableFile(t, w, filepath.Join(showDir, "tiny.rar"), make([]byte, 5))

	if err := w.scanDirectoryGroupByFolder(context.Background()); err != nil {
		t.Fatalf("scanDirectoryGroupByFolder: %v", err)
	}

	if folders := queuedFolderPaths(q); len(folders) != 0 {
		t.Errorf("expected 0 FOLDER entries (all files too small), got: %v", folders)
	}
}
