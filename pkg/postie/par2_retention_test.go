package postie

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/kipsilabs/postie/internal/transferwriter"
	"github.com/kipsilabs/postie/pkg/fileinfo"
)

// par2InDir is a PAR2 executor that always materialises a set in dir, mirroring
// the default behaviour (no output dir configured → temp/source dir).
type par2InDir struct{ dir string }

func (m *par2InDir) create(setName string) ([]string, error) {
	paths := []string{
		filepath.Join(m.dir, setName+".par2"),
		filepath.Join(m.dir, setName+".vol0+1.par2"),
	}
	for _, p := range paths {
		if err := os.WriteFile(p, []byte("par2"), 0644); err != nil {
			return nil, err
		}
	}
	return paths, nil
}

func (m *par2InDir) Create(_ context.Context, files []fileinfo.FileInfo) ([]string, error) {
	return m.create(filepath.Base(files[0].Path))
}

func (m *par2InDir) CreateInDirectory(_ context.Context, files []fileinfo.FileInfo, _ string) ([]string, error) {
	return m.create(filepath.Base(files[0].Path))
}

func (m *par2InDir) CreateSet(_ context.Context, _ []fileinfo.FileInfo, _, setName, _ string) ([]string, error) {
	return m.create(setName)
}

func par2FilesLeft(t *testing.T, dir string) int {
	t.Helper()
	matches, err := filepath.Glob(filepath.Join(dir, "*.par2"))
	if err != nil {
		t.Fatalf("glob: %v", err)
	}
	return len(matches)
}

type postFn func(p *Postie, ctx context.Context, files []fileinfo.FileInfo, root, out string) error

var postPaths = map[string]postFn{
	"postFolder": func(p *Postie, ctx context.Context, files []fileinfo.FileInfo, root, out string) error {
		_, err := p.postFolder(ctx, files, root, out)
		return err
	},
	"post": func(p *Postie, ctx context.Context, files []fileinfo.FileInfo, root, out string) error {
		_, err := p.post(ctx, files[0], root, out)
		return err
	},
	"postInParallel": func(p *Postie, ctx context.Context, files []fileinfo.FileInfo, root, out string) error {
		_, err := p.postInParallel(ctx, files[0], root, out)
		return err
	},
}

// TestPar2RetainedUntilVerifiedInDurableMode: with maintain_par2_files off the
// generated PAR2 set used to be deleted as soon as posting finished. In durable
// mode verification runs later in the background and may need to re-post PAR2
// articles from those files, and the transfer cleaner already removes them
// after verification. Deleting them eagerly turns any lost PAR2 article into a
// permanent verification failure for the whole transfer.
func TestPar2RetainedUntilVerifiedInDurableMode(t *testing.T) {
	for name, run := range postPaths {
		for _, waitForPar2 := range []bool{true, false} {
			t.Run(name, func(t *testing.T) {
				root := t.TempDir()
				files, cleanup := makeSourceFiles(t, root, "Folder", "video.mkv")
				defer cleanup()
				par2Dir := t.TempDir()

				p := newTestPostie(nil, waitForPar2, false)
				p.par2runner = &par2InDir{dir: par2Dir}
				p.recorder = transferwriter.New("transfer-1", t.TempDir(), nil)

				if err := run(p, context.Background(), files, root, t.TempDir()); err != nil {
					t.Fatalf("%s(waitForPar2=%v): %v", name, waitForPar2, err)
				}
				if n := par2FilesLeft(t, par2Dir); n != 2 {
					t.Errorf("%s(waitForPar2=%v): %d PAR2 files left, want 2 retained for verification", name, waitForPar2, n)
				}
			})
		}
	}
}

// TestPar2CleanedAfterPostInStandaloneMode keeps the legacy behaviour: without
// a durable recorder nothing else will remove the temporary PAR2 set.
func TestPar2CleanedAfterPostInStandaloneMode(t *testing.T) {
	for name, run := range postPaths {
		t.Run(name, func(t *testing.T) {
			root := t.TempDir()
			files, cleanup := makeSourceFiles(t, root, "Folder", "video.mkv")
			defer cleanup()
			par2Dir := t.TempDir()

			p := newTestPostie(nil, true, false)
			p.par2runner = &par2InDir{dir: par2Dir}

			if err := run(p, context.Background(), files, root, t.TempDir()); err != nil {
				t.Fatalf("%s: %v", name, err)
			}
			if n := par2FilesLeft(t, par2Dir); n != 0 {
				t.Errorf("%s: %d PAR2 files left, want 0 in standalone mode", name, n)
			}
		})
	}
}
