package postie

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/javi11/postie/internal/article"
	"github.com/javi11/postie/internal/nzb"
	"github.com/javi11/postie/internal/par2"
)

// par2FailingPoster posts every file's articles into the NZB generator like the
// real poster does as it goes, but fails any call that includes a PAR2 file —
// the shape of a PAR2 upload that dies after some of its articles are posted.
type par2FailingPoster struct{ mockPoster }

func (m *par2FailingPoster) Post(_ context.Context, files []string, _ string, nzbGen nzb.NZBGenerator) error {
	for i, f := range files {
		base := filepath.Base(f)
		nzbGen.AddArticle(&article.Article{
			MessageID:       base + "@test",
			OriginalSubject: "[1/1] \"" + base + "\" yEnc (1/1)",
			OriginalName:    base,
			FileName:        base,
			From:            "poster@test",
			Groups:          []string{"alt.binaries.test"},
			PartNumber:      1,
			TotalParts:      1,
			FileNumber:      i + 1,
			Size:            100,
		})
	}
	for _, f := range files {
		if par2.IsPar2File(f) {
			return errors.New("i/o timeout")
		}
	}
	return nil
}

func (m *par2FailingPoster) PostWithRelativePaths(ctx context.Context, files []string, root string, nzbGen nzb.NZBGenerator, rel map[string]string) error {
	return m.Post(ctx, files, root, nzbGen)
}

// TestPartialPar2UploadIsNotReferencedInNZB: in the parallel post paths a
// failed PAR2 upload is tolerated ("upload will continue without par2"), but
// the PAR2 articles posted before the failure were left in the NZB, producing
// an NZB that references an incomplete PAR2 set.
func TestPartialPar2UploadIsNotReferencedInNZB(t *testing.T) {
	paths := map[string]postFn{
		"postInParallel": postPaths["postInParallel"],
		"postFolder":     postPaths["postFolder"],
	}
	for name, run := range paths {
		t.Run(name, func(t *testing.T) {
			root := t.TempDir()
			files, cleanup := makeSourceFiles(t, root, "Folder", "video.mkv")
			defer cleanup()
			outDir := t.TempDir()

			p := newTestPostie(nil, false, false) // wait_for_par2 off → parallel branch
			p.par2runner = &par2InDir{dir: t.TempDir()}
			p.poster = &par2FailingPoster{}

			if err := run(p, context.Background(), files, root, outDir); err != nil {
				t.Fatalf("%s: %v", name, err)
			}

			var nzbs []string
			_ = filepath.Walk(outDir, func(path string, info os.FileInfo, err error) error {
				if err == nil && strings.HasSuffix(path, ".nzb") {
					nzbs = append(nzbs, path)
				}
				return nil
			})
			if len(nzbs) != 1 {
				t.Fatalf("%s: found %d NZBs, want 1", name, len(nzbs))
			}
			data, err := os.ReadFile(nzbs[0])
			if err != nil {
				t.Fatal(err)
			}
			if strings.Contains(string(data), ".par2") {
				t.Errorf("%s: NZB references PAR2 segments although the PAR2 upload failed:\n%s", name, data)
			}
			if !strings.Contains(string(data), "video.mkv") {
				t.Errorf("%s: NZB lost the main file", name)
			}
		})
	}
}
