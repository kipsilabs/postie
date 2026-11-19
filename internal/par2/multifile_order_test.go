package par2

import (
	"bytes"
	"context"
	"fmt"
	"math/rand"
	"os"
	"os/exec"
	"path/filepath"
	"testing"

	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/pkg/fileinfo"
)

// The PAR2 spec numbers a set's source slices in file-ID order (the order of the IDs in the Main
// packet). par2go sorts the IDs for the Main packet but encodes slices in input order, so a
// multi-file set whose inputs are not in ID order cannot be repaired when more than one block of the
// same file is damaged. CreateSet must hand the inputs over in ID order (see rust/DECISIONS.md, ADR-022).

func permutations(n int) [][]int {
	var out [][]int
	var rec func(cur []int, used []bool)
	rec = func(cur []int, used []bool) {
		if len(cur) == n {
			out = append(out, append([]int(nil), cur...))
			return
		}
		for i := 0; i < n; i++ {
			if !used[i] {
				used[i] = true
				rec(append(cur, i), used)
				used[i] = false
			}
		}
	}
	rec(nil, make([]bool, n))
	return out
}

func TestIntegration_CreateSet_MultiFileRepairsInEveryInputOrder(t *testing.T) {
	bin, err := exec.LookPath("par2")
	if err != nil {
		t.Skip("par2cmdline not installed")
	}

	for pi, perm := range permutations(3) {
		t.Run(fmt.Sprintf("order%d", pi), func(t *testing.T) {
			dir := t.TempDir()
			rng := rand.New(rand.NewSource(42))
			names := []string{"alpha.bin", "beta.bin", "gamma.bin"}
			orig := map[string][]byte{}
			var infos []fileinfo.FileInfo
			for _, n := range names {
				data := make([]byte, 300_000)
				rng.Read(data)
				p := filepath.Join(dir, n)
				if err := os.WriteFile(p, data, 0o644); err != nil {
					t.Fatal(err)
				}
				orig[n] = data
				infos = append(infos, fileinfo.FileInfo{Path: p, Size: uint64(len(data))})
			}
			ordered := make([]fileinfo.FileInfo, len(perm))
			for i, j := range perm {
				ordered[i] = infos[j]
			}

			cfg := &config.Par2Config{Redundancy: "50", SliceSize: 16_384, MemoryLimit: 1 << 30}
			ex := New(750_000, cfg, nil)
			if _, err := ex.CreateSet(context.Background(), ordered, dir, "set", dir); err != nil {
				t.Fatalf("CreateSet: %v", err)
			}

			// damage two blocks of one file: exponent 0 is a plain XOR, so a single damaged block
			// repairs even with a mismatched layout and would hide the bug
			victim := filepath.Join(dir, "beta.bin")
			dmg := append([]byte(nil), orig["beta.bin"]...)
			for _, off := range []int{1_000, 150_000, 290_000} {
				dmg[off] ^= 0xFF
			}
			if err := os.WriteFile(victim, dmg, 0o644); err != nil {
				t.Fatal(err)
			}

			cmd := exec.Command(bin, "r", "-q", filepath.Join(dir, "set.par2"))
			cmd.Dir = dir
			if out, err := cmd.CombinedOutput(); err != nil {
				t.Fatalf("par2 repair failed: %v\n%s", err, out)
			}
			got, err := os.ReadFile(victim)
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.Equal(got, orig["beta.bin"]) {
				t.Fatal("repaired file differs from the original")
			}
		})
	}
}
