// Package par2sweep removes orphaned PAR2 files from Postie's PAR2 work
// directory. Generated sets are normally deleted by the transfer cleaner once
// the transfer is verified, but when the transfer rows disappear first (a
// database reset, an old crash) nothing else ever removes them and the work
// dir grows by the redundancy share of every abandoned upload.
package par2sweep

import (
	"context"
	"log/slog"
	"os"
	"path/filepath"
	"time"

	"github.com/kipsilabs/postie/internal/par2"
)

// Sweeper deletes PAR2 files in dir that are older than grace, not referenced
// by any transfer and not in use by a running job. It only ever looks at the
// top level of dir, which must be a directory Postie owns exclusively.
type Sweeper struct {
	dir        string
	grace      time.Duration
	referenced func(context.Context) ([]string, error)
	inUse      func() []string
	now        func() time.Time
}

// New builds a Sweeper. referenced returns the source paths of every file the
// durable store still tracks (nil when there is no store); inUse returns the
// PAR2 files of jobs currently running (nil when not tracked).
func New(dir string, grace time.Duration, referenced func(context.Context) ([]string, error), inUse func() []string) *Sweeper {
	return &Sweeper{dir: dir, grace: grace, referenced: referenced, inUse: inUse, now: time.Now}
}

// Sweep runs one pass and returns how many files it removed. A failing
// referenced lookup aborts the pass without deleting anything, since orphans
// cannot be told apart from sets still awaiting verification.
func (s *Sweeper) Sweep(ctx context.Context) (int, error) {
	if s.dir == "" {
		return 0, nil
	}
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil
		}
		return 0, err
	}

	keep := map[string]struct{}{}
	if s.referenced != nil {
		paths, err := s.referenced(ctx)
		if err != nil {
			return 0, err
		}
		for _, p := range paths {
			keep[p] = struct{}{}
		}
	}
	if s.inUse != nil {
		for _, p := range s.inUse() {
			keep[p] = struct{}{}
		}
	}

	cutoff := s.now().Add(-s.grace)
	removed := 0
	for _, entry := range entries {
		if ctx.Err() != nil {
			return removed, ctx.Err()
		}
		if entry.IsDir() || !par2.IsPar2File(entry.Name()) {
			continue
		}
		path := filepath.Join(s.dir, entry.Name())
		if _, ok := keep[path]; ok {
			continue
		}
		info, err := entry.Info()
		if err != nil || !info.ModTime().Before(cutoff) {
			continue
		}
		if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
			slog.WarnContext(ctx, "Failed to remove orphaned PAR2 file", "path", path, "error", err)
			continue
		}
		slog.InfoContext(ctx, "Removed orphaned PAR2 file", "path", path, "age", s.now().Sub(info.ModTime()).Round(time.Minute))
		removed++
	}
	return removed, nil
}

// Run waits initialDelay, sweeps, and then sweeps every interval until ctx is
// cancelled. The delay gives jobs resumed at startup time to reserve the sets
// they are about to reuse before the first pass.
func (s *Sweeper) Run(ctx context.Context, initialDelay, interval time.Duration) {
	select {
	case <-ctx.Done():
		return
	case <-time.After(initialDelay):
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		if _, err := s.Sweep(ctx); err != nil && ctx.Err() == nil {
			slog.WarnContext(ctx, "PAR2 work dir sweep failed", "dir", s.dir, "error", err)
		}
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}
