package watcher

import (
	"context"
	"log/slog"
	"os"
	"slices"

	"github.com/kipsilabs/postie/internal/par2"
)

// siblingSetCache memoizes, for one scan, the reusable PAR2 set of each
// source file so a set's volumes do not re-read the same PAR2 header.
type siblingSetCache map[string][]string

func newSiblingSetCache() siblingSetCache { return siblingSetCache{} }

// postedWithSibling reports whether path is a PAR2 file that the job of its
// sibling source file will post. In one-NZB-per-file mode that job reuses the
// existing set on disk and puts it in the source's NZB, so queuing the PAR2
// files on their own would post every one of them twice (issue #184).
//
// It only says yes when the sibling will itself be queued (it exists and
// passes the size and ignore filters) and the set passes the same reuse check
// the posting job applies. Known limitation: if the sibling was already
// posted before its PAR2 files appeared, they are skipped and not posted.
func (w *Watcher) postedWithSibling(ctx context.Context, path string, cache siblingSetCache) bool {
	if !w.par2Enabled() || !par2.IsPar2File(path) {
		return false
	}
	source, ok := par2.SetSourceFor(path)
	if !ok {
		return false
	}

	set, cached := cache[source]
	if !cached {
		set = w.reusableSiblingSet(ctx, source)
		cache[source] = set
	}
	if !slices.Contains(set, path) {
		return false
	}

	slog.DebugContext(ctx, "PAR2 file is posted with its source file, not queuing it separately",
		"path", path, "source", source)
	return true
}

// reusableSiblingSet returns the PAR2 set the job for source will reuse, or
// nil when source will not be queued or has no reusable set.
func (w *Watcher) reusableSiblingSet(ctx context.Context, source string) []string {
	info, err := os.Stat(source)
	if err != nil || !info.Mode().IsRegular() || !w.passesFilters(source, info) {
		return nil
	}
	set, ok := par2.ReusableSetFor(ctx, source)
	if !ok {
		return nil
	}
	return set
}

func (w *Watcher) par2Enabled() bool {
	if w.par2Config == nil {
		return false
	}
	cfg := w.par2Config()
	return cfg != nil && cfg.Enabled != nil && *cfg.Enabled
}
