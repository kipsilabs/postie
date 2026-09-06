package processor

// Integration tests for the processor's folder-collection logic, covering bugs
// reported in github.com/kipsilabs/postie/issues/168.
//
// Run with: go test ./internal/processor/... -run "Integration" -v -timeout 60s

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// minimalProcessor returns a *Processor with only the fields needed by
// collectFilesInFolder (followSymlinks). All other fields are zero-valued.
func minimalProcessor(followSymlinks bool) *Processor {
	return &Processor{followSymlinks: followSymlinks}
}

// mkFile creates a file at path with size bytes of content.
func mkFile(t *testing.T, path string, size int) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		t.Fatalf("mkFile mkdir: %v", err)
	}
	data := make([]byte, size)
	for i := range data {
		data[i] = byte(i % 256)
	}
	if err := os.WriteFile(path, data, 0644); err != nil {
		t.Fatalf("mkFile write: %v", err)
	}
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// TestIntegration_CollectFilesInFolder_IncludesSubdirectoryFiles verifies that
// collectFilesInFolder recursively collects files from nested subdirectories.
// This exercises the key invariant needed for single-NZB-per-folder mode:
// all content under the folder root must be gathered into one job.
func TestIntegration_CollectFilesInFolder_IncludesSubdirectoryFiles(t *testing.T) {
	root := t.TempDir()
	p := minimalProcessor(false)

	// root/
	//   a.rar
	//   Proof/
	//     proof.jpg
	//   Sample/
	//     sample.mkv
	mkFile(t, filepath.Join(root, "a.rar"), 1024)
	mkFile(t, filepath.Join(root, "Proof", "proof.jpg"), 512)
	mkFile(t, filepath.Join(root, "Sample", "sample.mkv"), 2048)

	files, err := p.collectFilesInFolder(root)
	if err != nil {
		t.Fatalf("collectFilesInFolder: %v", err)
	}

	if len(files) != 3 {
		t.Fatalf("expected 3 files, got %d: %v", len(files), files)
	}

	// Verify all three paths are present.
	paths := make(map[string]bool, len(files))
	for _, f := range files {
		paths[f.Path] = true
	}
	for _, want := range []string{
		filepath.Join(root, "a.rar"),
		filepath.Join(root, "Proof", "proof.jpg"),
		filepath.Join(root, "Sample", "sample.mkv"),
	} {
		if !paths[want] {
			t.Errorf("expected %q in result, not found", want)
		}
	}
}

// TestIntegration_CollectFilesInFolder_MaintainsRelativePaths verifies that
// RelativePath for each file is set relative to the parent of the folder root
// and includes the folder name.  For example, if folderPath is /watch/SunMoments
// and a file is at /watch/SunMoments/Proof/proof.jpg, RelativePath must be
// "SunMoments/Proof/proof.jpg" (forward-slash normalised).
//
// This relative path is what deriveFolderName() uses to extract "SunMoments"
// as the NZB name — so correctness here is critical.
func TestIntegration_CollectFilesInFolder_MaintainsRelativePaths(t *testing.T) {
	watchDir := t.TempDir()
	folderName := "SunMoments"
	folderPath := filepath.Join(watchDir, folderName)
	p := minimalProcessor(false)

	mkFile(t, filepath.Join(folderPath, "01.rar"), 1024)
	mkFile(t, filepath.Join(folderPath, "Proof", "proof.jpg"), 512)
	mkFile(t, filepath.Join(folderPath, "Sample", "sample.mkv"), 2048)

	files, err := p.collectFilesInFolder(folderPath)
	if err != nil {
		t.Fatalf("collectFilesInFolder: %v", err)
	}
	if len(files) != 3 {
		t.Fatalf("expected 3 files, got %d", len(files))
	}

	for _, f := range files {
		if f.RelativePath == "" {
			t.Errorf("RelativePath is empty for %q", f.Path)
			continue
		}
		// Must be forward-slash normalised.
		if strings.Contains(f.RelativePath, "\\") {
			t.Errorf("RelativePath %q contains backslash — must be forward-slash", f.RelativePath)
		}
		// Must start with the folder name (so deriveFolderName returns it, not watchDir).
		if !strings.HasPrefix(f.RelativePath, folderName+"/") {
			t.Errorf("RelativePath %q does not start with %q — NZB would be misnamed", f.RelativePath, folderName+"/")
		}
	}
}

// TestIntegration_CollectFilesInFolder_SizeIsPopulated verifies that each returned
// FileInfo has a non-zero Size matching the file content.
func TestIntegration_CollectFilesInFolder_SizeIsPopulated(t *testing.T) {
	root := t.TempDir()
	p := minimalProcessor(false)

	mkFile(t, filepath.Join(root, "data.rar"), 8192)

	files, err := p.collectFilesInFolder(root)
	if err != nil {
		t.Fatalf("collectFilesInFolder: %v", err)
	}
	if len(files) != 1 {
		t.Fatalf("expected 1 file, got %d", len(files))
	}
	if files[0].Size != 8192 {
		t.Errorf("expected Size=8192, got %d", files[0].Size)
	}
}

// TestIntegration_CollectFilesInFolder_EmptyFolder verifies that an empty folder
// returns an empty slice without error.
func TestIntegration_CollectFilesInFolder_EmptyFolder(t *testing.T) {
	root := t.TempDir()
	p := minimalProcessor(false)

	files, err := p.collectFilesInFolder(root)
	if err != nil {
		t.Fatalf("collectFilesInFolder: %v", err)
	}
	if len(files) != 0 {
		t.Errorf("expected 0 files for empty folder, got %d: %v", len(files), files)
	}
}

// TestIntegration_CollectFilesInFolder_DeeplyNested verifies collection from
// deeply nested subdirectories.
func TestIntegration_CollectFilesInFolder_DeeplyNested(t *testing.T) {
	root := t.TempDir()
	p := minimalProcessor(false)

	// root/a/b/c/deep.rar
	mkFile(t, filepath.Join(root, "a", "b", "c", "deep.rar"), 256)
	mkFile(t, filepath.Join(root, "top.rar"), 256)

	files, err := p.collectFilesInFolder(root)
	if err != nil {
		t.Fatalf("collectFilesInFolder: %v", err)
	}
	if len(files) != 2 {
		t.Fatalf("expected 2 files, got %d: %v", len(files), files)
	}
}

// TestIntegration_CollectFilesInFolder_PathsAreFolderNamePrefixed verifies the
// deriveFolderName contract: that filepath.Base(files[0].RelativePath split by "/")[0]
// equals the folder name — which is what deriveFolderName() uses to name the NZB.
// Regression guard for github.com/kipsilabs/postie#190.
func TestIntegration_CollectFilesInFolder_PathsAreFolderNamePrefixed(t *testing.T) {
	watchDir := t.TempDir()
	folderName := "TheMovie2024"
	folderPath := filepath.Join(watchDir, folderName)
	p := minimalProcessor(false)

	mkFile(t, filepath.Join(folderPath, "part01.rar"), 512)
	mkFile(t, filepath.Join(folderPath, "Extras", "bonus.mkv"), 512)

	files, err := p.collectFilesInFolder(folderPath)
	if err != nil {
		t.Fatalf("collectFilesInFolder: %v", err)
	}
	if len(files) == 0 {
		t.Fatal("expected files, got none")
	}

	// Simulate deriveFolderName logic:
	//   relPath = filepath.Rel(watchDir, files[0].Path)   → "TheMovie2024/part01.rar"
	//   parts = strings.SplitN(relPath, "/", 2)           → ["TheMovie2024", "part01.rar"]
	//   derivedName = parts[0]                            → "TheMovie2024"
	relPath, err := filepath.Rel(watchDir, files[0].Path)
	if err != nil {
		t.Fatalf("Rel: %v", err)
	}
	parts := strings.SplitN(filepath.ToSlash(relPath), "/", 2)
	if len(parts) < 2 {
		t.Fatalf("relative path %q has fewer than 2 parts — deriveFolderName would fall back to watch-folder name", relPath)
	}
	derivedName := parts[0]
	if derivedName != folderName {
		t.Errorf("deriveFolderName would produce %q, expected %q — NZB would be misnamed", derivedName, folderName)
	}
	if derivedName == filepath.Base(watchDir) {
		t.Errorf("NZB name equals watch folder base name — #190 regression present")
	}
}
