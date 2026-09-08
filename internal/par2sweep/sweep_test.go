package par2sweep

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func writeAged(t *testing.T, path string, age time.Duration) {
	t.Helper()
	if err := os.WriteFile(path, []byte("x"), 0644); err != nil {
		t.Fatal(err)
	}
	mtime := time.Now().Add(-age)
	if err := os.Chtimes(path, mtime, mtime); err != nil {
		t.Fatal(err)
	}
}

func exists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

// Orphaned PAR2 sets accumulate in the shared work dir when the transfer rows
// are dropped (database reset) before the cleaner runs. The sweeper removes
// PAR2 files that are old, not referenced by any transfer and not in use by a
// running job, and nothing else.
func TestSweep(t *testing.T) {
	dir := t.TempDir()
	old := 48 * time.Hour

	orphan := filepath.Join(dir, "movie.mkv.par2")
	orphanVol := filepath.Join(dir, "movie.mkv.vol00+01.par2")
	referenced := filepath.Join(dir, "show.mkv.par2")
	inUse := filepath.Join(dir, "busy.mkv.vol00+01.par2")
	young := filepath.Join(dir, "fresh.mkv.par2")
	notPar2 := filepath.Join(dir, "notes.txt")
	for _, p := range []string{orphan, orphanVol, referenced, inUse, notPar2} {
		writeAged(t, p, old)
	}
	writeAged(t, young, time.Minute)
	if err := os.Mkdir(filepath.Join(dir, "sub.par2"), 0755); err != nil {
		t.Fatal(err)
	}

	s := New(dir, 24*time.Hour,
		func(context.Context) ([]string, error) { return []string{referenced}, nil },
		func() []string { return []string{inUse} },
	)
	removed, err := s.Sweep(context.Background())
	if err != nil {
		t.Fatalf("Sweep: %v", err)
	}
	if removed != 2 {
		t.Errorf("removed = %d, want 2", removed)
	}
	for _, p := range []string{orphan, orphanVol} {
		if exists(p) {
			t.Errorf("%s should have been removed", filepath.Base(p))
		}
	}
	for _, p := range []string{referenced, inUse, young, notPar2, filepath.Join(dir, "sub.par2")} {
		if !exists(p) {
			t.Errorf("%s must be kept", filepath.Base(p))
		}
	}
}

func TestSweep_NoDirIsNoop(t *testing.T) {
	for _, dir := range []string{"", filepath.Join(t.TempDir(), "missing")} {
		s := New(dir, time.Hour, nil, nil)
		if n, err := s.Sweep(context.Background()); err != nil || n != 0 {
			t.Errorf("dir %q: removed=%d err=%v", dir, n, err)
		}
	}
}

// When the referenced-paths lookup fails nothing may be deleted: we cannot tell
// orphans from sets still awaiting verification.
func TestSweep_KeepsEverythingWhenLookupFails(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "movie.mkv.par2")
	writeAged(t, p, 48*time.Hour)

	s := New(dir, time.Hour, func(context.Context) ([]string, error) { return nil, os.ErrClosed }, nil)
	if _, err := s.Sweep(context.Background()); err == nil {
		t.Fatal("expected error")
	}
	if !exists(p) {
		t.Fatal("file must survive a failed lookup")
	}
}

func TestRun_WaitsInitialDelayBeforeFirstSweep(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "movie.mkv.par2")
	writeAged(t, p, 48*time.Hour)

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	s := New(dir, time.Hour, nil, nil)
	go func() { s.Run(ctx, 200*time.Millisecond, time.Hour); close(done) }()

	time.Sleep(50 * time.Millisecond)
	if !exists(p) {
		t.Fatal("file removed before the initial delay elapsed")
	}
	deadline := time.Now().Add(5 * time.Second)
	for exists(p) && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	if exists(p) {
		t.Fatal("file not removed after the initial delay")
	}
	cancel()
	<-done
}
