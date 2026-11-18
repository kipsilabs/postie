package processor

import (
	"path/filepath"
	"slices"
	"testing"

	"github.com/kipsilabs/postie/pkg/fileinfo"
)

func TestOriginalsToDelete(t *testing.T) {
	dir := filepath.FromSlash("/watch/Release")
	src := filepath.Join(dir, "a.rar")
	sameDirPar2 := []string{filepath.Join(dir, "a.rar.par2"), filepath.Join(dir, "a.rar.vol00+01.par2")}
	workDirPar2 := filepath.FromSlash("/tmp/postie-par2/a.rar.par2")

	got := originalsToDelete(
		[]fileinfo.FileInfo{{Path: src}, {Path: sameDirPar2[0]}},
		append(slices.Clone(sameDirPar2), workDirPar2),
	)
	want := []string{src, sameDirPar2[0], sameDirPar2[1]}
	if !slices.Equal(got, want) {
		t.Fatalf("originalsToDelete = %v, want %v", got, want)
	}
}
