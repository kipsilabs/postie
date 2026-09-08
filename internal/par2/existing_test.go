package par2

import (
	"context"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/javi11/par2go"
	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/pkg/fileinfo"
)

// writeRealPar2Files generates a genuine PAR2 set for sourcePath into dir using
// par2go, so the FileDesc packets describe sourcePath. With NumRecovery=1 the
// volume is named "<base>.vol00+01.par2". Returns the main file path.
func writeRealPar2Files(t *testing.T, dir, sourcePath string, numRecovery int) string {
	t.Helper()
	main := filepath.Join(dir, filepath.Base(sourcePath)+".par2")
	opts := par2go.Options{SliceSize: 4096, NumRecovery: numRecovery, NumGoroutines: 1, Creator: "test"}
	if err := par2go.Create(context.Background(), main, []string{sourcePath}, opts); err != nil {
		t.Fatalf("writeRealPar2Files: %v", err)
	}
	return main
}

// writeRealPar2Set generates a genuine PAR2 set named setName for inputs.
func writeRealPar2Set(t *testing.T, dir, setName string, inputs []par2go.InputFile) string {
	t.Helper()
	main := filepath.Join(dir, setName+".par2")
	opts := par2go.Options{SliceSize: 4096, NumRecovery: 1, NumGoroutines: 1, Creator: "test"}
	if err := par2go.CreateWithNames(context.Background(), main, inputs, opts); err != nil {
		t.Fatalf("writeRealPar2Set: %v", err)
	}
	return main
}

func touch(t *testing.T, path string) {
	t.Helper()
	if err := os.WriteFile(path, []byte("x"), 0644); err != nil {
		t.Fatal(err)
	}
}

// Regression for kipsilabs/postie#184 (last comment): volumes were collected
// with a loose prefix match, so any "<base>*.vol*.par2" file in a shared temp
// dir (e.g. from a different upload whose name starts with this one) was
// pulled into the NZB.
func TestPar2SetOnDisk_StrictNameMatching(t *testing.T) {
	dir := t.TempDir()
	want := []string{"movie.mkv.par2", "movie.mkv.vol00+01.par2", "movie.mkv.vol01+02.par2", "movie.mkv.vol123+456.par2"}
	decoys := []string{
		"movie.mkv.1080p.vol00+01.par2", // other upload sharing the prefix
		"movie.mkv.PROPER.par2",
		"movie.mkv.volatile.par2",
		"movie.mkvx.vol00+01.par2",
		"movie.mkv.vol00+01.par2.tmp",
		"movie.mkv.vol.par2",
		"other.mkv.vol00+01.par2",
	}
	for _, n := range slices.Concat(want, decoys) {
		touch(t, filepath.Join(dir, n))
	}
	if err := os.Mkdir(filepath.Join(dir, "movie.mkv.vol02+03.par2"), 0755); err != nil {
		t.Fatal(err)
	}

	got := par2SetOnDisk(context.Background(), dir, "movie.mkv")
	var names []string
	for _, p := range got {
		names = append(names, filepath.Base(p))
	}
	slices.Sort(names)
	slices.Sort(want)
	if !slices.Equal(names, want) {
		t.Fatalf("par2SetOnDisk = %v, want %v", names, want)
	}
}

func TestReadFileDescs_MatchesSource(t *testing.T) {
	dir := t.TempDir()
	src := filepath.Join(dir, "data.bin")
	createTestFile(t, src, 50000)
	main := writeRealPar2Files(t, dir, src, 1)

	descs, err := readFileDescs(main)
	if err != nil {
		t.Fatalf("readFileDescs: %v", err)
	}
	if len(descs) != 1 {
		t.Fatalf("expected 1 FileDesc, got %d", len(descs))
	}
	want, err := describeSource(src, "data.bin")
	if err != nil {
		t.Fatal(err)
	}
	if descs[0] != want {
		t.Fatalf("FileDesc = %+v, want %+v", descs[0], want)
	}
}

func TestReadFileDescs_RejectsGarbage(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "x.par2")
	touch(t, p)
	if _, err := readFileDescs(p); err == nil {
		t.Fatal("expected error for non-PAR2 content")
	}
}

// Regression for kipsilabs/postie#184 (last comment): a PAR2 set left behind in
// the shared temp dir by an earlier upload (the transfer rows were dropped, so
// the cleaner never removed it) was reused for any later file with the same
// basename, even though it described different content.
func TestCheckExistingPar2Files_RejectsPar2ForDifferentContent(t *testing.T) {
	ctx := context.Background()
	sourceDir := t.TempDir()
	tempDir := t.TempDir()
	staleDir := t.TempDir()

	// Stale set: same basename and size, different bytes.
	stale := filepath.Join(staleDir, "archive.rar")
	if err := os.WriteFile(stale, make([]byte, 50000), 0644); err != nil {
		t.Fatal(err)
	}
	writeRealPar2Files(t, tempDir, stale, 1)

	src := filepath.Join(sourceDir, "archive.rar")
	createTestFile(t, src, 50000)

	exec := New(10000, &config.Par2Config{Redundancy: "10", TempDir: tempDir}, nil)
	file := fileinfo.FileInfo{Path: src, Size: 50000}

	if paths, ok := exec.checkExistingPar2Files(ctx, file); ok {
		t.Fatalf("stale PAR2 for different content must not be reused, got %v", paths)
	}

	// A set that really describes the source is reused.
	writeRealPar2Files(t, tempDir, src, 1)
	paths, ok := exec.checkExistingPar2Files(ctx, file)
	if !ok || len(paths) != 2 {
		t.Fatalf("matching PAR2 should be reused, ok=%v paths=%v", ok, paths)
	}
}

func TestCheckExistingPar2Set_RejectsStaleSet(t *testing.T) {
	ctx := context.Background()
	folder := t.TempDir()
	tempDir := t.TempDir()

	a := filepath.Join(folder, "a.bin")
	b := filepath.Join(folder, "b.bin")
	createTestFile(t, a, 20000)
	createTestFile(t, b, 30000)
	inputs := []par2go.InputFile{{Path: a, Name: "a.bin"}, {Path: b, Name: "b.bin"}}
	writeRealPar2Set(t, tempDir, "folder", inputs)

	if paths, ok := checkExistingPar2SetInPath(ctx, "folder", tempDir, inputs); !ok || len(paths) != 2 {
		t.Fatalf("matching set should be reused, ok=%v paths=%v", ok, paths)
	}

	// Folder gained a file: the old set no longer covers it.
	c := filepath.Join(folder, "c.bin")
	createTestFile(t, c, 10000)
	grown := append(slices.Clone(inputs), par2go.InputFile{Path: c, Name: "c.bin"})
	if paths, ok := checkExistingPar2SetInPath(ctx, "folder", tempDir, grown); ok {
		t.Fatalf("set missing a file must not be reused, got %v", paths)
	}

	// Folder lost a file: the old set describes something that is not posted.
	if paths, ok := checkExistingPar2SetInPath(ctx, "folder", tempDir, inputs[:1]); ok {
		t.Fatalf("set with extra files must not be reused, got %v", paths)
	}

	// Same names, different content.
	createTestFile(t, a, 20001)
	if paths, ok := checkExistingPar2SetInPath(ctx, "folder", tempDir, inputs); ok {
		t.Fatalf("set for changed content must not be reused, got %v", paths)
	}
}

// Callers must be able to tell PAR2 files Postie wrote (which it may delete
// later) from pre-existing ones it merely reused (kipsilabs/postie#274).
func TestResult_DistinguishesReusedFromCreated(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	src := filepath.Join(dir, "video.mkv")
	createTestFile(t, src, 50000)
	exec := New(10000, &config.Par2Config{Redundancy: "10"}, nil)
	files := []fileinfo.FileInfo{{Path: src, Size: 50000}}

	res, err := exec.CreateInDirectory(ctx, files, "")
	if err != nil {
		t.Fatal(err)
	}
	if len(res.Created) == 0 || len(res.Reused) != 0 {
		t.Fatalf("fresh generation: Created=%v Reused=%v", res.Created, res.Reused)
	}

	res, err = exec.CreateInDirectory(ctx, files, "")
	if err != nil {
		t.Fatal(err)
	}
	if len(res.Reused) == 0 || len(res.Created) != 0 {
		t.Fatalf("second run must reuse: Created=%v Reused=%v", res.Created, res.Reused)
	}
	if got := res.All(); len(got) != len(res.Reused) {
		t.Fatalf("All() = %v", got)
	}
}

// all flattens a Result for tests that only care about the posted paths.
func all(r Result, err error) ([]string, error) { return r.All(), err }
