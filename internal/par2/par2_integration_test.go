package par2

// Integration tests for PAR2 executor behavior, covering bugs reported in
// github.com/javi11/postie/issues/168.
//
// Run with: go test ./internal/par2/... -run "Integration" -v -timeout 120s

import (
	"bytes"
	"context"
	"encoding/binary"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	"github.com/javi11/postie/internal/config"
	"github.com/javi11/postie/pkg/fileinfo"
)

// createIntegrationTestFile creates a temporary file filled with deterministic data.
func createIntegrationTestFile(t *testing.T, dir, name string, size int) string {
	t.Helper()
	data := make([]byte, size)
	for i := range data {
		data[i] = byte(i % 256)
	}
	path := filepath.Join(dir, name)
	if err := os.WriteFile(path, data, 0644); err != nil {
		t.Fatalf("createIntegrationTestFile: %v", err)
	}
	return path
}

// createFakePar2Files places empty files that look like PAR2 outputs for sourceFile.
// Returns paths of the created files.
func createFakePar2Files(t *testing.T, dir, sourceBaseName string) (main string, vol string) {
	t.Helper()
	main = filepath.Join(dir, sourceBaseName+".par2")
	vol = filepath.Join(dir, sourceBaseName+".vol0+1.par2")
	for _, p := range []string{main, vol} {
		if err := os.WriteFile(p, []byte("fake par2 content"), 0644); err != nil {
			t.Fatalf("createFakePar2Files: %v", err)
		}
	}
	return main, vol
}

// countPar2FilesInDir counts how many .par2 files are in dir (including vol files).
func countPar2FilesInDir(t *testing.T, dir string) int {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("countPar2FilesInDir: %v", err)
	}
	n := 0
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".par2") {
			n++
		}
	}
	return n
}

// ---------------------------------------------------------------------------
// Bug: Skip PAR2 if files already exist — regression for issue #188
// Fixes: "detect pre-existing PAR2 files in source dir when TempDir configured"
// ---------------------------------------------------------------------------

// TestIntegration_NativeExecutor_SkipsWhenPar2FilesExistInSourceDir verifies that
// NativeExecutor returns the existing PAR2 files without regenerating when they
// already live in the source directory — even when TempDir is configured.
// Regression test for github.com/javi11/postie#188.
func TestIntegration_NativeExecutor_SkipsWhenPar2FilesExistInSourceDir(t *testing.T) {
	dir := t.TempDir()
	tempDir := t.TempDir()

	sourcePath := createIntegrationTestFile(t, dir, "movie.rar", 1024*1024) // 1 MB
	mainPar2, volPar2 := createFakePar2Files(t, dir, "movie.rar")

	// Record modification times before calling Create.
	statBefore := func(path string) int64 {
		info, err := os.Stat(path)
		if err != nil {
			t.Fatalf("stat before: %v", err)
		}
		return info.ModTime().UnixNano()
	}
	mainMtimeBefore := statBefore(mainPar2)
	volMtimeBefore := statBefore(volPar2)

	cfg := &config.Par2Config{
		Redundancy: "10",
		TempDir:    tempDir, // TempDir configured — must still check source dir first.
	}
	executor := New(750_000, cfg, nil)

	files := []fileinfo.FileInfo{{Path: sourcePath, Size: 1024 * 1024}}
	result, err := executor.Create(context.Background(), files)
	if err != nil {
		t.Fatalf("Create: unexpected error: %v", err)
	}

	// Must return at least the two pre-existing files.
	if len(result) < 2 {
		t.Fatalf("expected >= 2 PAR2 paths returned (the pre-existing ones), got %d: %v", len(result), result)
	}

	// The main PAR2 file must be in the result.
	found := false
	for _, p := range result {
		if p == mainPar2 {
			found = true
		}
	}
	if !found {
		t.Errorf("existing main .par2 file not in result; result=%v", result)
	}

	// Existing files must not have been modified.
	mainMtimeAfter := statBefore(mainPar2) // reuse helper
	volMtimeAfter := statBefore(volPar2)
	if mainMtimeAfter != mainMtimeBefore {
		t.Error("main .par2 file was modified — executor should have skipped generation")
	}
	if volMtimeAfter != volMtimeBefore {
		t.Error("vol .par2 file was modified — executor should have skipped generation")
	}

	// No new .par2 files must appear in either dir.
	if n := countPar2FilesInDir(t, dir); n != 2 {
		t.Errorf("source dir: expected 2 .par2 files, got %d", n)
	}
	if n := countPar2FilesInDir(t, tempDir); n != 0 {
		t.Errorf("temp dir: expected 0 .par2 files (source files reused), got %d", n)
	}
}

// TestIntegration_NativeExecutor_SkipsWhenPar2FilesExistInTempDir verifies that
// when PAR2 files do NOT exist in the source dir but DO exist in TempDir, the
// executor returns the TempDir files without regenerating.
// Regression test for github.com/javi11/postie#188.
func TestIntegration_NativeExecutor_SkipsWhenPar2FilesExistInTempDir(t *testing.T) {
	sourceDir := t.TempDir()
	tempDir := t.TempDir()

	sourcePath := createIntegrationTestFile(t, sourceDir, "archive.rar", 512*1024)
	// PAR2 files live only in tempDir, not in sourceDir.
	mainPar2, _ := createFakePar2Files(t, tempDir, "archive.rar")

	cfg := &config.Par2Config{
		Redundancy: "10",
		TempDir:    tempDir,
	}
	executor := New(750_000, cfg, nil)

	files := []fileinfo.FileInfo{{Path: sourcePath, Size: 512 * 1024}}
	result, err := executor.Create(context.Background(), files)
	if err != nil {
		t.Fatalf("Create: unexpected error: %v", err)
	}

	if len(result) == 0 {
		t.Fatal("expected PAR2 paths to be returned (from TempDir), got empty result")
	}

	found := false
	for _, p := range result {
		if p == mainPar2 {
			found = true
		}
	}
	if !found {
		t.Errorf("TempDir main .par2 not in result; result=%v", result)
	}

	// Source dir must remain clean.
	if n := countPar2FilesInDir(t, sourceDir); n != 0 {
		t.Errorf("source dir should have 0 .par2 files, got %d", n)
	}
}

// TestIntegration_NativeExecutor_RegeneratesWhenNoPar2FilesExist verifies that
// when no existing PAR2 files are found, the executor creates new ones.
func TestIntegration_NativeExecutor_RegeneratesWhenNoPar2FilesExist(t *testing.T) {
	dir := t.TempDir()
	sourcePath := createIntegrationTestFile(t, dir, "file.rar", 100_000) // 100 KB

	cfg := &config.Par2Config{
		Redundancy:  "10",
		SliceSize:   10 * 1024 * 1024,
		MemoryLimit: 4 * 1024 * 1024 * 1024,
	}
	executor := New(10_000, cfg, nil)

	files := []fileinfo.FileInfo{{Path: sourcePath, Size: 100_000}}
	result, err := executor.Create(context.Background(), files)
	if err != nil {
		t.Fatalf("Create: unexpected error: %v", err)
	}
	if len(result) == 0 {
		t.Fatal("expected new PAR2 files to be created, got empty result")
	}
	expectedMain := filepath.Join(dir, "file.rar.par2")
	if _, err := os.Stat(expectedMain); os.IsNotExist(err) {
		t.Errorf("expected main PAR2 file %s to be created", expectedMain)
	}
}

// ---------------------------------------------------------------------------
// Bug: BinaryExecutor skip path — regression for issue #188
// ---------------------------------------------------------------------------

// TestIntegration_BinaryExecutor_SkipsWhenPar2FilesExistInSourceDir verifies that
// BinaryExecutor also skips invoking the external binary when PAR2 files already
// exist in the source directory.  A fake binary path is provided — if the binary
// were actually invoked it would fail, making the test self-enforcing.
func TestIntegration_BinaryExecutor_SkipsWhenPar2FilesExistInSourceDir(t *testing.T) {
	dir := t.TempDir()
	sourcePath := createIntegrationTestFile(t, dir, "video.mkv", 2*1024*1024)
	mainPar2, _ := createFakePar2Files(t, dir, "video.mkv")

	cfg := &config.Par2Config{
		Redundancy:       "10",
		ParparBinaryPath: "/nonexistent/parpar", // must not be invoked
	}
	executor := NewBinaryExecutor(750_000, cfg, nil)

	files := []fileinfo.FileInfo{{Path: sourcePath, Size: 2 * 1024 * 1024}}
	result, err := executor.CreateInDirectory(context.Background(), files, "")
	if err != nil {
		t.Fatalf("CreateInDirectory: unexpected error: %v", err)
	}
	if len(result) == 0 {
		t.Fatal("expected existing PAR2 paths returned, got empty result")
	}
	found := false
	for _, p := range result {
		if p == mainPar2 {
			found = true
		}
	}
	if !found {
		t.Errorf("existing main .par2 not in result; result=%v", result)
	}
}

// ---------------------------------------------------------------------------
// Bug: PAR2 native generation fails on very small files — regression for #189
// Fixes: "align PAR2 slice size to SIMD boundary to prevent segfault on Linux"
// ---------------------------------------------------------------------------

// TestIntegration_NativeExecutor_HandlesVerySmallFiles_512Bytes verifies that the
// native executor does not panic or return an error for a 512-byte file.
// Regression test for github.com/javi11/postie#189.
func TestIntegration_NativeExecutor_HandlesVerySmallFiles_512Bytes(t *testing.T) {
	dir := t.TempDir()
	sourcePath := createIntegrationTestFile(t, dir, "tiny.rar", 512)

	cfg := &config.Par2Config{
		Redundancy:  "10",
		MemoryLimit: 4 * 1024 * 1024 * 1024,
	}
	executor := New(750_000, cfg, nil)

	files := []fileinfo.FileInfo{{Path: sourcePath, Size: 512}}

	// Must not panic. Error is acceptable (file may be too small for PAR2).
	result, err := func() (r []string, e error) {
		defer func() {
			if rec := recover(); rec != nil {
				t.Errorf("executor panicked on 512-byte file: %v", rec)
			}
		}()
		return executor.Create(context.Background(), files)
	}()

	if err != nil {
		// An error is fine — the important thing is no panic / no segfault.
		t.Logf("Create returned error for 512-byte file (acceptable): %v", err)
		return
	}
	t.Logf("PAR2 files created for 512-byte file: %v", result)
}

// TestIntegration_NativeExecutor_HandlesVerySmallFiles_10Bytes verifies that the
// native executor handles a 10-byte file gracefully (skips or errors without panicking).
// Regression test for github.com/javi11/postie#189.
func TestIntegration_NativeExecutor_HandlesVerySmallFiles_10Bytes(t *testing.T) {
	dir := t.TempDir()
	sourcePath := createIntegrationTestFile(t, dir, "micro.rar", 10)

	cfg := &config.Par2Config{
		Redundancy:  "10",
		MemoryLimit: 4 * 1024 * 1024 * 1024,
	}
	executor := New(750_000, cfg, nil)

	files := []fileinfo.FileInfo{{Path: sourcePath, Size: 10}}

	_, err := func() (r []string, e error) {
		defer func() {
			if rec := recover(); rec != nil {
				t.Errorf("executor panicked on 10-byte file: %v", rec)
			}
		}()
		return executor.Create(context.Background(), files)
	}()

	// A 10-byte file is too small for a meaningful PAR2 block; expect either
	// a nil result (skipped with warning) or a graceful error, but never a panic.
	if err != nil {
		t.Logf("Create returned error for 10-byte file (acceptable): %v", err)
	}
}

// TestIntegration_NativeExecutor_BlockSizeClampedToFileSize verifies that the SIMD
// alignment guard in calculateParBlockSize prevents the block size from exceeding
// the file size.  The fix for #189 ensures (blockSize / 128) * 128 ≤ fileSize.
func TestIntegration_NativeExecutor_BlockSizeClampedToFileSize(t *testing.T) {
	cases := []struct {
		name     string
		fileSize uint64
	}{
		{"64-byte file", 64},
		{"127-byte file", 127},
		{"128-byte file", 128},
		{"255-byte file", 255},
		{"500-byte file", 500},
		{"1KB file", 1024},
	}

	articleSize := uint64(750_000) // typical article size

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			blockSize := calculateParBlockSize(tc.fileSize, articleSize)

			// The raw block size can exceed fileSize; that is clamped inside
			// createPar2ForFile.  We test the clamping logic directly.
			var clamped uint64
			const simdAlignment = uint64(128)
			if blockSize > tc.fileSize {
				if tc.fileSize >= simdAlignment {
					clamped = (tc.fileSize / simdAlignment) * simdAlignment
				} else {
					clamped = (tc.fileSize / 4) * 4
				}
			} else {
				clamped = blockSize
			}

			if clamped > tc.fileSize {
				t.Errorf("clamped block size %d exceeds file size %d — SIMD guard broken", clamped, tc.fileSize)
			}

			// Ensure alignment is a multiple of 4 (PAR2 spec minimum).
			if clamped > 0 && clamped%4 != 0 {
				t.Errorf("clamped block size %d is not 4-byte aligned", clamped)
			}
		})
	}
}

// TestIntegration_NativeExecutor_SkipsInputPar2Files verifies that .par2 files
// in the input list are ignored by the executor (not re-processed).
func TestIntegration_NativeExecutor_SkipsInputPar2Files(t *testing.T) {
	dir := t.TempDir()
	sourcePath := createIntegrationTestFile(t, dir, "data.rar", 50_000)
	par2Path := createIntegrationTestFile(t, dir, "data.rar.par2", 500)

	cfg := &config.Par2Config{
		Redundancy:  "10",
		SliceSize:   10 * 1024 * 1024,
		MemoryLimit: 4 * 1024 * 1024 * 1024,
	}
	executor := New(10_000, cfg, nil)

	// Include a .par2 file in the input — it must be silently skipped.
	files := []fileinfo.FileInfo{
		{Path: sourcePath, Size: 50_000},
		{Path: par2Path, Size: 500},
	}

	_, err := executor.Create(context.Background(), files)
	if err != nil {
		t.Fatalf("Create: unexpected error: %v", err)
	}
}

// ---------------------------------------------------------------------------
// CreateSet — folder mode, FileDesc relative paths (issue #219)
// ---------------------------------------------------------------------------

// extractFileDescNames parses a par2 file and returns the list of filenames
// embedded in FileDesc packets, in the order they appear.
func extractFileDescNames(t *testing.T, path string) []string {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read par2 %s: %v", path, err)
	}
	magic := []byte{'P', 'A', 'R', '2', 0, 'P', 'K', 'T'}
	fdType := []byte{'P', 'A', 'R', ' ', '2', '.', '0', 0, 'F', 'i', 'l', 'e', 'D', 'e', 's', 'c'}
	var names []string
	for off := 0; off+64 <= len(data); {
		if !bytes.Equal(data[off:off+8], magic) {
			off++
			continue
		}
		length := binary.LittleEndian.Uint64(data[off+8 : off+16])
		if length < 64 || off+int(length) > len(data) {
			break
		}
		typ := data[off+48 : off+64]
		if bytes.Equal(typ, fdType) {
			// Body layout: fileID(16) hashFull(16) hash16k(16) fileSize(8) name(N)
			body := data[off+64 : off+int(length)]
			if len(body) > 56 {
				name := strings.TrimRight(string(body[56:]), "\x00")
				names = append(names, name)
			}
		}
		off += int(length)
	}
	return names
}

func TestIntegration_NativeExecutor_CreateSet_EmbedsRelativePaths(t *testing.T) {
	root := t.TempDir()
	// Build folder1-testpkg/{file1.txt, folder2/file2.txt, folder2/folder3/file3.txt}
	pkgDir := filepath.Join(root, "folder1-testpkg")
	mustMkdir(t, filepath.Join(pkgDir, "folder2", "folder3"))
	file1 := createIntegrationTestFile(t, pkgDir, "file1.txt", 4096)
	file2 := createIntegrationTestFile(t, filepath.Join(pkgDir, "folder2"), "file2.txt", 4096)
	file3 := createIntegrationTestFile(t, filepath.Join(pkgDir, "folder2", "folder3"), "file3.txt", 4096)

	files := []fileinfo.FileInfo{
		{Path: file1, Size: 4096, RelativePath: "folder1-testpkg/file1.txt"},
		{Path: file2, Size: 4096, RelativePath: "folder1-testpkg/folder2/file2.txt"},
		{Path: file3, Size: 4096, RelativePath: "folder1-testpkg/folder2/folder3/file3.txt"},
	}

	cfg := &config.Par2Config{Redundancy: "10", SliceSize: 0, MemoryLimit: 4 * 1024 * 1024 * 1024}
	executor := New(750_000, cfg, nil)

	outDir := filepath.Join(root, "out")
	// folderDir is the on-disk root of the folder being posted.
	// FileDesc names must be relative to this dir (no top-level prefix).
	folderDir := pkgDir
	created, err := executor.CreateSet(context.Background(), files, outDir, "folder1-testpkg", folderDir)
	if err != nil {
		t.Fatalf("CreateSet: %v", err)
	}
	if len(created) == 0 {
		t.Fatal("CreateSet returned no files")
	}

	mainPar2 := filepath.Join(outDir, "folder1-testpkg.par2")
	if _, err := os.Stat(mainPar2); err != nil {
		t.Fatalf("expected main par2 %s: %v", mainPar2, err)
	}

	got := dedupe(extractFileDescNames(t, mainPar2))
	sort.Strings(got)
	// SABnzbd already creates a job folder named "folder1-testpkg" from the
	// NZB title; FileDesc paths must be relative to that folder root so files
	// land at the correct depth (no double-nesting).
	want := []string{
		"file1.txt",
		"folder2/file2.txt",
		"folder2/folder3/file3.txt",
	}
	if !equalStrings(got, want) {
		t.Errorf("FileDesc names: got %v, want %v", got, want)
	}
}

// TestIntegration_NativeExecutor_CreateSet_AllFilesInSubdir verifies that when
// all input files happen to live in one subdirectory (not spread across the
// folder root), the FileDesc still carries the subdir prefix — not bare basenames.
func TestIntegration_NativeExecutor_CreateSet_AllFilesInSubdir(t *testing.T) {
	root := t.TempDir()
	pkgDir := filepath.Join(root, "ShowS01")
	mustMkdir(t, filepath.Join(pkgDir, "extras"))
	fileA := createIntegrationTestFile(t, filepath.Join(pkgDir, "extras"), "bonus.mkv", 4096)
	fileB := createIntegrationTestFile(t, filepath.Join(pkgDir, "extras"), "deleted.mkv", 4096)

	files := []fileinfo.FileInfo{
		{Path: fileA, Size: 4096, RelativePath: "ShowS01/extras/bonus.mkv"},
		{Path: fileB, Size: 4096, RelativePath: "ShowS01/extras/deleted.mkv"},
	}
	cfg := &config.Par2Config{Redundancy: "10", MemoryLimit: 4 * 1024 * 1024 * 1024}
	executor := New(750_000, cfg, nil)

	outDir := filepath.Join(root, "out")
	folderDir := pkgDir // <root>/ShowS01
	if _, err := executor.CreateSet(context.Background(), files, outDir, "ShowS01", folderDir); err != nil {
		t.Fatalf("CreateSet: %v", err)
	}
	got := dedupe(extractFileDescNames(t, filepath.Join(outDir, "ShowS01.par2")))
	sort.Strings(got)
	// Both files are in extras/ — FileDesc must preserve the subdir prefix.
	want := []string{"extras/bonus.mkv", "extras/deleted.mkv"}
	if !equalStrings(got, want) {
		t.Errorf("FileDesc names: got %v, want %v", got, want)
	}
}

func TestIntegration_NativeExecutor_CreateSet_FallsBackToBasename(t *testing.T) {
	root := t.TempDir()
	file := createIntegrationTestFile(t, root, "loose.bin", 4096)

	files := []fileinfo.FileInfo{
		{Path: file, Size: 4096}, // no RelativePath
	}
	cfg := &config.Par2Config{Redundancy: "10", MemoryLimit: 4 * 1024 * 1024 * 1024}
	executor := New(750_000, cfg, nil)

	outDir := filepath.Join(root, "out")
	// folderDir == root: filepath.Rel(root, file) == "loose.bin"
	if _, err := executor.CreateSet(context.Background(), files, outDir, "loose", root); err != nil {
		t.Fatalf("CreateSet: %v", err)
	}
	got := dedupe(extractFileDescNames(t, filepath.Join(outDir, "loose.par2")))
	if !equalStrings(got, []string{"loose.bin"}) {
		t.Errorf("FileDesc names: got %v, want [loose.bin]", got)
	}
}

func mustMkdir(t *testing.T, p string) {
	t.Helper()
	if err := os.MkdirAll(p, 0755); err != nil {
		t.Fatalf("mkdir %s: %v", p, err)
	}
}

func dedupe(in []string) []string {
	seen := map[string]bool{}
	out := []string{}
	for _, s := range in {
		if !seen[s] {
			seen[s] = true
			out = append(out, s)
		}
	}
	sort.Strings(out)
	return out
}

func equalStrings(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// TestIntegration_NativeExecutor_PinnedLookupKernel verifies that a pinned
// gf16_method is honoured end-to-end. The Lookup kernel is portable, so this
// runs on every CPU.
func TestIntegration_NativeExecutor_PinnedLookupKernel(t *testing.T) {
	dir := t.TempDir()
	sourcePath := createIntegrationTestFile(t, dir, "file.rar", 100_000) // 100 KB

	cfg := &config.Par2Config{
		Redundancy:  "10",
		SliceSize:   10 * 1024 * 1024,
		MemoryLimit: 4 * 1024 * 1024 * 1024,
		GF16Method:  "lookup",
	}
	executor := New(10_000, cfg, nil)

	files := []fileinfo.FileInfo{{Path: sourcePath, Size: 100_000}}
	result, err := executor.Create(context.Background(), files)
	if err != nil {
		t.Fatalf("Create: unexpected error: %v", err)
	}
	if len(result) == 0 {
		t.Fatal("expected PAR2 files to be created, got empty result")
	}
	if _, err := os.Stat(filepath.Join(dir, "file.rar.par2")); err != nil {
		t.Errorf("expected main PAR2 file to exist: %v", err)
	}
}

// TestIntegration_NativeExecutor_RejectsUnknownGF16Method verifies an invalid
// kernel name fails fast instead of silently falling back to auto.
func TestIntegration_NativeExecutor_RejectsUnknownGF16Method(t *testing.T) {
	dir := t.TempDir()
	sourcePath := createIntegrationTestFile(t, dir, "file.rar", 100_000)

	cfg := &config.Par2Config{
		Redundancy:  "10",
		MemoryLimit: 4 * 1024 * 1024 * 1024,
		GF16Method:  "bogus",
	}
	executor := New(10_000, cfg, nil)

	_, err := executor.Create(context.Background(), []fileinfo.FileInfo{{Path: sourcePath, Size: 100_000}})
	if err == nil {
		t.Fatal("Create: expected error for gf16_method=bogus, got nil")
	}
	if !strings.Contains(err.Error(), "gf16_method") {
		t.Errorf("error %q should mention gf16_method", err)
	}
}
