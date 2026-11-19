package par2

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"

	"github.com/javi11/par2go"
)

func TestSortInputsByFileID_IsOrderIndependentAndAscending(t *testing.T) {
	dir := t.TempDir()
	var inputs []par2go.InputFile
	for i, n := range []string{"c.bin", "a.bin", "b.bin", "d.bin"} {
		p := filepath.Join(dir, n)
		if err := os.WriteFile(p, bytes.Repeat([]byte{byte(i + 1)}, 1000+i), 0o644); err != nil {
			t.Fatal(err)
		}
		inputs = append(inputs, par2go.InputFile{Path: p, Name: n})
	}

	want := sortInputsByFileID(inputs)
	for _, perm := range permutations(len(inputs)) {
		shuffled := make([]par2go.InputFile, len(inputs))
		for i, j := range perm {
			shuffled[i] = inputs[j]
		}
		got := sortInputsByFileID(shuffled)
		for i := range got {
			if got[i] != want[i] {
				t.Fatalf("perm %v: position %d = %s, want %s", perm, i, got[i].Name, want[i].Name)
			}
		}
	}

	var prev [16]byte
	for i, in := range want {
		id, err := par2FileID(in)
		if err != nil {
			t.Fatal(err)
		}
		if i > 0 && bytes.Compare(prev[:], id[:]) >= 0 {
			t.Fatalf("IDs not ascending at %d", i)
		}
		prev = id
	}
	// the caller's slice is not modified
	if inputs[0].Name != "c.bin" {
		t.Fatal("input slice was mutated")
	}
}

func TestSortInputsByFileID_UnreadableInputKeepsOrder(t *testing.T) {
	in := []par2go.InputFile{{Path: "/no/such/b"}, {Path: "/no/such/a"}}
	got := sortInputsByFileID(in)
	if got[0].Path != "/no/such/b" || got[1].Path != "/no/such/a" {
		t.Fatalf("order changed: %v", got)
	}
}
