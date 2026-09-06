package postie

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/kipsilabs/postie/pkg/fileinfo"
)

// withinDir checks whether path is inside dir.
func withinDir(path, dir string) bool {
	cleanPath := filepath.Clean(path)
	cleanDir := filepath.Clean(dir) + string(filepath.Separator)
	return strings.HasPrefix(cleanPath, cleanDir)
}

// TestPostCrossVolumeNZBInOutputDir verifies that non-folder mode (post/postInParallel)
// places the NZB in the output directory, not the watch (source) directory, even when
// the two are on different volumes (no shared path prefix).
func TestPostCrossVolumeNZBInOutputDir(t *testing.T) {
	watchDir := t.TempDir()
	outputDir := t.TempDir()

	// Create a source file in the watch directory
	srcFile := filepath.Join(watchDir, "movie.mkv")
	if err := os.WriteFile(srcFile, []byte("content"), 0644); err != nil {
		t.Fatalf("write source file: %v", err)
	}

	par2mock := &mockPar2Executor{}
	p := newTestPostie(par2mock, true, false)

	// post() is the sequential (waitForPar2=true) non-folder path
	nzbPath, err := p.post(context.Background(), fileinfo.FileInfo{
		Path: srcFile,
		Size: 7,
	}, watchDir, outputDir)
	if err != nil {
		t.Fatalf("post() returned error: %v", err)
	}

	// NZB must be inside the output directory
	if !withinDir(nzbPath, outputDir) {
		t.Errorf("NZB placed outside output dir:\n  nzbPath:   %s\n  outputDir: %s", nzbPath, outputDir)
	}

	// NZB must NOT be in the watch directory
	if withinDir(nzbPath, watchDir) {
		t.Errorf("NZB leaked into watch dir:\n  nzbPath:  %s\n  watchDir: %s", nzbPath, watchDir)
	}

	// Verify the file actually exists
	if _, err := os.Stat(nzbPath); os.IsNotExist(err) {
		t.Errorf("NZB file does not exist at %q", nzbPath)
	}
}

// TestPostInParallelCrossVolumeNZBInOutputDir does the same check for the parallel path.
func TestPostInParallelCrossVolumeNZBInOutputDir(t *testing.T) {
	watchDir := t.TempDir()
	outputDir := t.TempDir()

	srcFile := filepath.Join(watchDir, "movie.mkv")
	if err := os.WriteFile(srcFile, []byte("content"), 0644); err != nil {
		t.Fatalf("write source file: %v", err)
	}

	par2mock := &mockPar2Executor{}
	p := newTestPostie(par2mock, false, false)

	nzbPath, err := p.postInParallel(context.Background(), fileinfo.FileInfo{
		Path: srcFile,
		Size: 7,
	}, watchDir, outputDir)
	if err != nil {
		t.Fatalf("postInParallel() returned error: %v", err)
	}

	if !withinDir(nzbPath, outputDir) {
		t.Errorf("NZB placed outside output dir:\n  nzbPath:   %s\n  outputDir: %s", nzbPath, outputDir)
	}

	if withinDir(nzbPath, watchDir) {
		t.Errorf("NZB leaked into watch dir:\n  nzbPath:  %s\n  watchDir: %s", nzbPath, watchDir)
	}

	if _, err := os.Stat(nzbPath); os.IsNotExist(err) {
		t.Errorf("NZB file does not exist at %q", nzbPath)
	}
}

// TestPostCrossVolumeWithSubdirectory verifies that files in a subdirectory of the
// watch folder maintain their relative path structure in the output directory.
func TestPostCrossVolumeWithSubdirectory(t *testing.T) {
	watchDir := t.TempDir()
	outputDir := t.TempDir()

	// Create a source file in a subdirectory of the watch folder
	subDir := filepath.Join(watchDir, "subfolder")
	if err := os.MkdirAll(subDir, 0755); err != nil {
		t.Fatalf("mkdir subfolder: %v", err)
	}
	srcFile := filepath.Join(subDir, "movie.mkv")
	if err := os.WriteFile(srcFile, []byte("content"), 0644); err != nil {
		t.Fatalf("write source file: %v", err)
	}

	par2mock := &mockPar2Executor{}
	p := newTestPostie(par2mock, true, false)

	nzbPath, err := p.post(context.Background(), fileinfo.FileInfo{
		Path: srcFile,
		Size: 7,
	}, watchDir, outputDir)
	if err != nil {
		t.Fatalf("post() returned error: %v", err)
	}

	// NZB should be in outputDir/subfolder/
	expectedDir := filepath.Join(outputDir, "subfolder")
	if filepath.Dir(nzbPath) != expectedDir {
		t.Errorf("NZB not in expected subdirectory:\n  got:  %s\n  want: %s/", filepath.Dir(nzbPath), expectedDir)
	}
}
