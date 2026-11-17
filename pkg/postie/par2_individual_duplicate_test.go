package postie

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/javi11/nzbparser"
	"github.com/kipsilabs/postie/internal/pool"
	"github.com/kipsilabs/postie/pkg/fileinfo"
)

// In individual-file mode every source file gets its own NZB. A pre-existing
// PAR2 set that is part of the source list must be posted through its own NZB
// only, not attached again to the NZB of the file it protects.
func TestIntegration_IndividualModeSourcePar2PostedOnce(t *testing.T) {
	for _, wait := range []bool{true, false} {
		t.Run(fmt.Sprintf("wait=%t", wait), func(t *testing.T) {
			ctx := context.Background()
			root := t.TempDir()
			folder := filepath.Join(root, "Release")
			if err := os.MkdirAll(folder, 0755); err != nil {
				t.Fatal(err)
			}
			file := writeBenchFile(t, folder, "payload.rar", 1)
			file.RelativePath = "Release/payload.rar"
			srv := startSinkNNTPServer(t, false)
			cfg := benchConfig(srv.port(), 2, true, wait, t.TempDir())
			cfg.Servers[0].SSL = false
			cfg.Servers[0].InsecureSSL = false
			cfg.Par2.MaintainPar2Files = boolPtr(true)
			cfg.Par2.SkipIfPar2Exists = boolPtr(true)
			pm, err := pool.New(cfg)
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() { _ = pm.Close() })

			post := func(files []fileinfo.FileInfo) {
				t.Helper()
				p, err := New(ctx, cfg, pm, noopProgress{}, nil)
				if err != nil {
					t.Fatal(err)
				}
				defer p.Close()
				if _, err := p.Post(ctx, files, root, root, false); err != nil {
					t.Fatal(err)
				}
			}

			// Run 1 generates the PAR2 set next to the source.
			post([]fileinfo.FileInfo{file})
			par2s, err := filepath.Glob(filepath.Join(folder, "*.par2"))
			if err != nil || len(par2s) == 0 {
				t.Fatalf("expected generated PAR2 files, got %v (err %v)", par2s, err)
			}
			nzbs, _ := filepath.Glob(filepath.Join(folder, "*.nzb"))
			for _, n := range nzbs {
				if err := os.Remove(n); err != nil {
					t.Fatal(err)
				}
			}

			// Run 2 posts the media file together with its existing PAR2 files.
			files := []fileinfo.FileInfo{file}
			for _, path := range par2s {
				info, err := os.Stat(path)
				if err != nil {
					t.Fatal(err)
				}
				files = append(files, fileinfo.FileInfo{Path: path, Size: uint64(info.Size()), RelativePath: "Release/" + filepath.Base(path)})
			}
			post(files)

			nzbs, err = filepath.Glob(filepath.Join(folder, "*.nzb"))
			if err != nil {
				t.Fatal(err)
			}
			if len(nzbs) != len(files) {
				t.Fatalf("got %d NZBs, want one per source file (%d)", len(nzbs), len(files))
			}
			seen := map[string]int{}
			for _, n := range nzbs {
				data, err := os.ReadFile(n)
				if err != nil {
					t.Fatal(err)
				}
				parsed, err := nzbparser.ParseString(string(data))
				if err != nil {
					t.Fatal(err)
				}
				for _, f := range parsed.Files {
					seen[f.Filename]++
				}
			}
			if len(seen) != len(files) {
				t.Errorf("combined NZBs reference %d distinct files, want %d: %v", len(seen), len(files), seen)
			}
			for name, c := range seen {
				if c != 1 {
					t.Errorf("%q appears in %d NZB entries, want 1", name, c)
				}
			}
		})
	}
}
