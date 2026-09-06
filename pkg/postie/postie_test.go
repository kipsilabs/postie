package postie

import (
	"testing"

	"github.com/kipsilabs/postie/pkg/fileinfo"
)

func TestDeriveFolderName(t *testing.T) {
	tests := []struct {
		name     string
		rootDir  string
		files    []fileinfo.FileInfo
		expected string
	}{
		{
			name:    "watch mode single nzb per folder - one level nested",
			rootDir: "/data/watch",
			files: []fileinfo.FileInfo{
				{Path: "/data/watch/MyShow/ep1.mkv"},
			},
			expected: "MyShow",
		},
		{
			name:    "watch mode single nzb per folder - deeply nested",
			rootDir: "/data/watch",
			files: []fileinfo.FileInfo{
				{Path: "/data/watch/MyShow/Season1/ep1.mkv"},
			},
			expected: "MyShow",
		},
		{
			name:    "add folder button - parent is rootDir",
			rootDir: "/data/watch",
			files: []fileinfo.FileInfo{
				{Path: "/data/watch/MyShow/ep1.mkv"},
				{Path: "/data/watch/MyShow/ep2.mkv"},
			},
			expected: "MyShow",
		},
		{
			// Note: this edge case does NOT occur in normal watcher operation.
			// When SingleNzbPerFolder=true, files directly in the watch root are
			// processed individually (one NZB per file) and never reach postFolder.
			// This fallback only triggers if someone runs "Add Folder" on the watch
			// folder itself rather than on a subfolder inside it.
			name:    "files directly in rootDir - fallback to base name",
			rootDir: "/data/watch",
			files: []fileinfo.FileInfo{
				{Path: "/data/watch/ep1.mkv"},
			},
			expected: "watch",
		},
		{
			name:    "rootDir is root slash - fallback to upload",
			rootDir: "/",
			files: []fileinfo.FileInfo{
				{Path: "/ep1.mkv"},
			},
			expected: "upload",
		},
		{
			name:     "empty files list - fallback to base name",
			rootDir:  "/data/watch",
			files:    []fileinfo.FileInfo{},
			expected: "watch",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := deriveFolderName(tt.rootDir, tt.files)
			if got != tt.expected {
				t.Errorf("deriveFolderName(%q, files) = %q, want %q", tt.rootDir, got, tt.expected)
			}
		})
	}
}
