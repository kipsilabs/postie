package postie

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/javi11/postie/internal/article"
	"github.com/javi11/postie/internal/config"
	"github.com/javi11/postie/internal/nzb"
	"github.com/javi11/postie/internal/poster"
	"github.com/javi11/postie/pkg/fileinfo"
)

// ─── mock poster ────────────────────────────────────────────────────────────

type mockPoster struct{}

func (m *mockPoster) Post(_ context.Context, files []string, _ string, nzbGen nzb.NZBGenerator) error {
	addFakeArticles(nzbGen, files)
	return nil
}

func (m *mockPoster) PostWithRelativePaths(_ context.Context, files []string, _ string, nzbGen nzb.NZBGenerator, _ map[string]string) error {
	addFakeArticles(nzbGen, files)
	return nil
}

func (m *mockPoster) Stats() poster.StatsSnapshot { return poster.StatsSnapshot{} }
func (m *mockPoster) Close()                      {}

// addFakeArticles injects one minimal article per file so nzbGen.Generate succeeds.
func addFakeArticles(nzbGen nzb.NZBGenerator, files []string) {
	for i, f := range files {
		a := &article.Article{
			MessageID:       "fake-id@test",
			OriginalSubject: "test subject",
			OriginalName:    filepath.Base(f),
			FileName:        filepath.Base(f),
			From:            "poster@test",
			Groups:          []string{"alt.binaries.test"},
			PartNumber:      1,
			TotalParts:      1,
			FileNumber:      i + 1,
			Size:            100,
		}
		nzbGen.AddArticle(a)
	}
}

// ─── mock PAR2 executor ──────────────────────────────────────────────────────

type mockPar2Executor struct {
	// recordedOutputDir is set on each CreateInDirectory call.
	recordedOutputDir string
	// par2FileNames are created in outputDir when it is non-empty.
	par2FileNames []string
}

func (m *mockPar2Executor) Create(_ context.Context, _ []fileinfo.FileInfo) ([]string, error) {
	return nil, nil
}

func (m *mockPar2Executor) CreateInDirectory(_ context.Context, _ []fileinfo.FileInfo, outputDir string) ([]string, error) {
	m.recordedOutputDir = outputDir
	if outputDir == "" || len(m.par2FileNames) == 0 {
		return nil, nil
	}
	if err := os.MkdirAll(outputDir, 0755); err != nil {
		return nil, err
	}
	var created []string
	for _, name := range m.par2FileNames {
		p := filepath.Join(outputDir, name)
		if err := os.WriteFile(p, []byte("dummy"), 0644); err != nil {
			return nil, err
		}
		created = append(created, p)
	}
	return created, nil
}

func (m *mockPar2Executor) CreateSet(ctx context.Context, files []fileinfo.FileInfo, outputDir, _, _ string) ([]string, error) {
	return m.CreateInDirectory(ctx, files, outputDir)
}

// ─── helpers ────────────────────────────────────────────────────────────────

func boolPtr(b bool) *bool { return &b }

// newTestPostie builds a minimal Postie with the given PAR2 and poster mocks.
func newTestPostie(par2exec *mockPar2Executor, waitForPar2 bool, maintainPar2 bool) *Postie {
	return &Postie{
		par2Cfg: &config.Par2Config{
			Enabled:           boolPtr(true),
			MaintainPar2Files: boolPtr(maintainPar2),
		},
		postingCfg: config.PostingConfig{
			WaitForPar2:        boolPtr(waitForPar2),
			ArticleSizeInBytes: 750_000,
		},
		compressionCfg: config.NzbCompressionConfig{Enabled: false},
		par2runner:     par2exec,
		poster:         &mockPoster{},
	}
}

// makeSourceFiles creates a temporary source folder with a dummy file and returns
// the folder path, the file list, and a cleanup function.
func makeSourceFiles(t *testing.T, watchRoot, folderName, fileName string) ([]fileinfo.FileInfo, func()) {
	t.Helper()
	folderPath := filepath.Join(watchRoot, folderName)
	if err := os.MkdirAll(folderPath, 0755); err != nil {
		t.Fatalf("mkdir source folder: %v", err)
	}
	filePath := filepath.Join(folderPath, fileName)
	if err := os.WriteFile(filePath, []byte("content"), 0644); err != nil {
		t.Fatalf("write source file: %v", err)
	}
	files := []fileinfo.FileInfo{{
		Path:         filePath,
		Size:         7,
		RelativePath: folderName + "/" + fileName,
	}}
	return files, func() { _ = os.RemoveAll(watchRoot) }
}

// ─── tests ───────────────────────────────────────────────────────────────────

// TestPostFolderOutputSubdirectory verifies that postFolder always places the
// NZB inside <outputDir>/<folderName>/ regardless of whether the watch folder
// and output folder are on the same or different volume paths.
func TestPostFolderOutputSubdirectory(t *testing.T) {
	tests := []struct {
		name        string
		watchRoot   string // simulated watch folder root
		folderName  string
		waitForPar2 bool
	}{
		{
			name:        "same-volume paths, sequential (WaitForPar2=true)",
			folderName:  "Movie_A",
			waitForPar2: true,
		},
		{
			name:        "same-volume paths, parallel (WaitForPar2=false)",
			folderName:  "Movie_A",
			waitForPar2: false,
		},
		{
			name:        "cross-volume paths, sequential (WaitForPar2=true)",
			folderName:  "Movie_A",
			waitForPar2: true,
		},
		{
			name:        "cross-volume paths, parallel (WaitForPar2=false)",
			folderName:  "Movie_A",
			waitForPar2: false,
		},
		{
			name:        "folder with nested content",
			folderName:  "TV.Show.S01",
			waitForPar2: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			watchRoot := t.TempDir()
			outputDir := t.TempDir()

			files, cleanup := makeSourceFiles(t, watchRoot, tt.folderName, "movie.mkv")
			defer cleanup()

			par2mock := &mockPar2Executor{} // maintain_par2_files = false
			p := newTestPostie(par2mock, tt.waitForPar2, false)

			// rootDir is the parent of the folder being processed
			rootDir := watchRoot
			_, err := p.postFolder(context.Background(), files, rootDir, outputDir)
			if err != nil {
				t.Fatalf("postFolder returned error: %v", err)
			}

			wantNZB := filepath.Join(outputDir, tt.folderName, tt.folderName+".nzb")
			if _, err := os.Stat(wantNZB); os.IsNotExist(err) {
				t.Errorf("NZB not found at expected path %q", wantNZB)
			}

			// NZB must NOT be in the output root (old broken behaviour)
			wrongNZB := filepath.Join(outputDir, tt.folderName+".nzb")
			if _, err := os.Stat(wrongNZB); err == nil {
				t.Errorf("NZB found at old (incorrect) path %q — should be in subfolder", wrongNZB)
			}
		})
	}
}

// TestPostFolderMaintainPar2FilesSubdirectory verifies that when maintain_par2_files
// is enabled ("nzb peer file" mode), PAR2 files are also placed in the same
// <outputDir>/<folderName>/ subdirectory as the NZB — not in the output root.
func TestPostFolderMaintainPar2FilesSubdirectory(t *testing.T) {
	tests := []struct {
		name        string
		folderName  string
		waitForPar2 bool
		par2Names   []string
	}{
		{
			name:        "maintain_par2_files enabled, sequential (WaitForPar2=true)",
			folderName:  "Movie_A",
			waitForPar2: true,
			par2Names:   []string{"movie.mkv.par2", "movie.mkv.vol0+1.par2"},
		},
		{
			name:        "maintain_par2_files enabled, parallel (WaitForPar2=false)",
			folderName:  "Movie_A",
			waitForPar2: false,
			par2Names:   []string{"movie.mkv.par2", "movie.mkv.vol0+1.par2"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			watchRoot := t.TempDir()
			outputDir := t.TempDir()

			files, cleanup := makeSourceFiles(t, watchRoot, tt.folderName, "movie.mkv")
			defer cleanup()

			par2mock := &mockPar2Executor{par2FileNames: tt.par2Names}
			p := newTestPostie(par2mock, tt.waitForPar2, true) // maintainPar2=true

			rootDir := watchRoot
			_, err := p.postFolder(context.Background(), files, rootDir, outputDir)
			if err != nil {
				t.Fatalf("postFolder returned error: %v", err)
			}

			wantSubdir := filepath.Join(outputDir, tt.folderName)

			// NZB must be in the subfolder
			wantNZB := filepath.Join(wantSubdir, tt.folderName+".nzb")
			if _, err := os.Stat(wantNZB); os.IsNotExist(err) {
				t.Errorf("NZB not found at expected subfolder path %q", wantNZB)
			}

			// PAR2 executor must have received the subfolder as outputDir
			if par2mock.recordedOutputDir != wantSubdir {
				t.Errorf("par2 executor received outputDir=%q, want %q",
					par2mock.recordedOutputDir, wantSubdir)
			}

			// Each PAR2 file must be inside the subfolder, not in the output root
			for _, name := range tt.par2Names {
				wantPar2 := filepath.Join(wantSubdir, name)
				if _, err := os.Stat(wantPar2); os.IsNotExist(err) {
					t.Errorf("PAR2 file %q not found in expected subfolder", wantPar2)
				}
				wrongPar2 := filepath.Join(outputDir, name)
				if _, err := os.Stat(wrongPar2); err == nil {
					t.Errorf("PAR2 file %q found in output root (should be in subfolder)", wrongPar2)
				}
			}
		})
	}
}

// TestPostFolderCrossVolumePathSeparation verifies the specific cross-volume
// scenario: watch folder on one "volume" path and output on another.
// The key invariant is that rootDir and files[0].Path share a prefix that is
// NOT a prefix of outputDir — simulating different disk volumes.
func TestPostFolderCrossVolumePathSeparation(t *testing.T) {
	// Simulate cross-volume by using two completely independent temp dirs
	// (on the same real host FS, but with no shared path prefix after the
	// OS temp root, mimicking the cross-volume case at the path-string level).
	vol3Watch := t.TempDir()  // simulates /volume3/Watch
	vol2Output := t.TempDir() // simulates /volume2/output

	const folderName = "Movie_A"
	files, cleanup := makeSourceFiles(t, vol3Watch, folderName, "movie.mkv")
	defer cleanup()

	par2mock := &mockPar2Executor{}
	p := newTestPostie(par2mock, false, false)

	_, err := p.postFolder(context.Background(), files, vol3Watch, vol2Output)
	if err != nil {
		t.Fatalf("postFolder returned error: %v", err)
	}

	wantNZB := filepath.Join(vol2Output, folderName, folderName+".nzb")
	if _, err := os.Stat(wantNZB); os.IsNotExist(err) {
		t.Errorf("cross-volume: NZB not found at %q", wantNZB)
	}

	// Confirm nothing leaked into the output root
	entries, _ := os.ReadDir(vol2Output)
	for _, e := range entries {
		if !e.IsDir() {
			t.Errorf("unexpected file in output root: %q (all files should be in subfolder)", e.Name())
		}
		if e.IsDir() && e.Name() != folderName {
			t.Errorf("unexpected directory in output root: %q", e.Name())
		}
	}
}
