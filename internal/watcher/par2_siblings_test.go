package watcher

// Regression tests for github.com/kipsilabs/postie/issues/184: with
// SingleNzbPerFolder off, a source file's job reuses its existing PAR2 set and
// posts it inside the source's NZB. The watcher must not also queue those PAR2
// files as their own jobs, or every PAR2 ends up in two NZBs.

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/par2"
	"github.com/kipsilabs/postie/pkg/fileinfo"
)

func individualWatcher(t *testing.T, watchDir string, par2Enabled bool) (*Watcher, *mockQueueWithDuplicateCheck) {
	t.Helper()
	cfg := config.WatcherConfig{
		Enabled:        true,
		WatchDirectory: watchDir,
		MinFileSize:    10,
		CheckInterval:  config.Duration("1s"),
		MinFileAge:     config.Duration("2s"),
	}
	enabled := par2Enabled
	par2Cfg := &config.Par2Config{Enabled: &enabled}
	q := &mockQueueWithDuplicateCheck{addFileCalls: make([]string, 0)}
	w := New(cfg, q, nil, watchDir, WithPar2Config(func() *config.Par2Config { return par2Cfg }))
	return w, q
}

// writePar2Set creates source with the given content and a real per-file PAR2
// set next to it, then makes every file pass the watcher's stability check.
func writePar2Set(t *testing.T, w *Watcher, source string, content []byte) []string {
	t.Helper()
	createStableFile(t, w, source, content)
	exec := par2.New(10000, &config.Par2Config{Redundancy: "10"}, nil)
	res, err := exec.CreateInDirectory(context.Background(),
		[]fileinfo.FileInfo{{Path: source, Size: uint64(len(content))}}, "")
	if err != nil {
		t.Fatalf("create par2: %v", err)
	}
	if len(res.Created) < 2 {
		t.Fatalf("expected a main PAR2 file plus volumes, got %v", res.Created)
	}
	for _, p := range res.Created {
		data, err := os.ReadFile(p)
		if err != nil {
			t.Fatal(err)
		}
		createStableFile(t, w, p, data)
	}
	return res.Created
}

func scanIndividual(t *testing.T, w *Watcher, q *mockQueueWithDuplicateCheck) []string {
	t.Helper()
	if err := w.scanDirectory(context.Background()); err != nil {
		t.Fatalf("scanDirectory: %v", err)
	}
	got := queuedIndividualPaths(q)
	slices.Sort(got)
	return got
}

func TestIndividualMode_ExistingPar2PostedOnlyWithSource(t *testing.T) {
	dir := t.TempDir()
	w, q := individualWatcher(t, dir, true)

	a := filepath.Join(dir, "release.part1.rar")
	b := filepath.Join(dir, "release.part2.rar")
	writePar2Set(t, w, a, bytes.Repeat([]byte("a"), 50000))
	writePar2Set(t, w, b, bytes.Repeat([]byte("b"), 50000))

	got := scanIndividual(t, w, q)
	want := []string{a, b}
	if !slices.Equal(got, want) {
		t.Fatalf("queued %v, want only the source files %v", got, want)
	}
}

func TestIndividualMode_Par2QueuedWhenSourceWillNotPostIt(t *testing.T) {
	tests := []struct {
		name  string
		setup func(t *testing.T, w *Watcher, dir string) (source string, par2s []string)
		par2  bool
	}{
		{
			name: "par2 disabled",
			par2: false,
			setup: func(t *testing.T, w *Watcher, dir string) (string, []string) {
				src := filepath.Join(dir, "movie.mkv")
				return src, writePar2Set(t, w, src, bytes.Repeat([]byte("m"), 50000))
			},
		},
		{
			name: "set describes other data",
			par2: true,
			setup: func(t *testing.T, w *Watcher, dir string) (string, []string) {
				src := filepath.Join(dir, "movie.mkv")
				par2s := writePar2Set(t, w, src, bytes.Repeat([]byte("m"), 50000))
				createStableFile(t, w, src, bytes.Repeat([]byte("x"), 50000))
				return src, par2s
			},
		},
		{
			name: "source ignored",
			par2: true,
			setup: func(t *testing.T, w *Watcher, dir string) (string, []string) {
				src := filepath.Join(dir, "movie.mkv")
				par2s := writePar2Set(t, w, src, bytes.Repeat([]byte("m"), 50000))
				w.cfg.IgnorePatterns = []string{"*.mkv"}
				return "", par2s
			},
		},
		{
			name: "orphan par2 without source",
			par2: true,
			setup: func(t *testing.T, w *Watcher, dir string) (string, []string) {
				src := filepath.Join(dir, "movie.mkv")
				par2s := writePar2Set(t, w, src, bytes.Repeat([]byte("m"), 50000))
				if err := os.Remove(src); err != nil {
					t.Fatal(err)
				}
				return "", par2s
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			w, q := individualWatcher(t, dir, tt.par2)
			source, par2s := tt.setup(t, w, dir)

			want := slices.Clone(par2s)
			if source != "" {
				want = append(want, source)
			}
			slices.Sort(want)
			if got := scanIndividual(t, w, q); !slices.Equal(got, want) {
				t.Fatalf("queued %v, want %v", got, want)
			}
		})
	}
}

func TestGroupMode_RootFilesSkipPar2PostedWithSource(t *testing.T) {
	dir := t.TempDir()
	w, q := individualWatcher(t, dir, true)
	w.cfg.SingleNzbPerFolder = true

	src := filepath.Join(dir, "loose.bin")
	writePar2Set(t, w, src, bytes.Repeat([]byte("l"), 50000))

	got := scanIndividual(t, w, q)
	if !slices.Equal(got, []string{src}) {
		t.Fatalf("queued %v, want only %s", got, src)
	}
	for _, p := range got {
		if strings.HasSuffix(p, ".par2") {
			t.Fatalf("PAR2 file %s queued separately", p)
		}
	}
}
