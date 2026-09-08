package postie

import (
	"context"
	"path/filepath"
	"slices"
	"testing"

	"github.com/kipsilabs/postie/internal/par2"
)

// Every PAR2 file a running job posts, created or reused, must be reported as
// in use until the job closes, so the sweeper never deletes a set that is
// still being posted. A reused set in the work dir may be an old orphan that
// no transfer row protects yet.
func TestPar2InUseTrackedUntilClose(t *testing.T) {
	for name, mk := range map[string]func(dir string) par2.Par2Executor{
		"created": func(dir string) par2.Par2Executor { return &par2InDir{dir: dir} },
		"reused":  func(dir string) par2.Par2Executor { return &par2Reused{dir: dir} },
	} {
		t.Run(name, func(t *testing.T) {
			rt := &Runtime{}
			root := t.TempDir()
			files, cleanup := makeSourceFiles(t, root, "Folder", "video.mkv")
			defer cleanup()
			par2Dir := t.TempDir()

			p := newTestPostie(nil, true, true) // maintain_par2_files: keep files so reservation is observable
			p.par2runner = mk(par2Dir)
			p.par2Reserver = rt

			if _, err := p.post(context.Background(), files[0], root, t.TempDir()); err != nil {
				t.Fatal(err)
			}
			inUse := rt.Par2InUse()
			slices.Sort(inUse)
			want := []string{filepath.Join(par2Dir, "video.mkv.par2"), filepath.Join(par2Dir, "video.mkv.vol0+1.par2")}
			if !slices.Equal(inUse, want) {
				t.Fatalf("Par2InUse = %v, want %v", inUse, want)
			}

			p.Close()
			if left := rt.Par2InUse(); len(left) != 0 {
				t.Fatalf("after Close Par2InUse = %v, want none", left)
			}
		})
	}
}
