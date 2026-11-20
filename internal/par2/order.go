package par2

import (
	"bytes"
	"crypto/md5"
	"encoding/binary"
	"io"
	"os"
	"path/filepath"
	"slices"

	"github.com/javi11/par2go"
)

// The PAR2 spec numbers a set's source slices in the order of the file IDs in the Main packet
// (ascending). par2go sorts the IDs for the Main packet but encodes recovery data in input order, so a
// multi-file set whose inputs are not already in ID order carries recovery blocks that do not match
// the declared layout and cannot repair more than one damaged block of a file. Handing the inputs over
// in ID order removes the mismatch without touching par2go.

// par2FileID computes the PAR2 file ID of an input exactly as par2go does:
// MD5(MD5(first 16 KiB) || size as little-endian uint64 || name).
func par2FileID(in par2go.InputFile) ([16]byte, error) {
	var id [16]byte
	f, err := os.Open(in.Path)
	if err != nil {
		return id, err
	}
	defer func() { _ = f.Close() }()
	st, err := f.Stat()
	if err != nil {
		return id, err
	}
	size := uint64(st.Size())
	var hash16k [16]byte
	if size > 0 {
		buf := make([]byte, min(size, hash16kLen))
		if _, err := io.ReadFull(f, buf); err != nil {
			return id, err
		}
		hash16k = md5.Sum(buf)
	}
	name := in.Name
	if name == "" {
		name = filepath.Base(in.Path)
	}
	h := md5.New()
	h.Write(hash16k[:])
	_ = binary.Write(h, binary.LittleEndian, size)
	h.Write([]byte(name))
	copy(id[:], h.Sum(nil))
	return id, nil
}

// sortInputsByFileID returns the inputs ordered by ascending file ID. When an input cannot be read the
// original order is returned unchanged: par2go reports that error itself.
func sortInputsByFileID(inputs []par2go.InputFile) []par2go.InputFile {
	type keyed struct {
		in par2go.InputFile
		id [16]byte
	}
	items := make([]keyed, len(inputs))
	for i, in := range inputs {
		id, err := par2FileID(in)
		if err != nil {
			return inputs
		}
		items[i] = keyed{in: in, id: id}
	}
	slices.SortStableFunc(items, func(a, b keyed) int { return bytes.Compare(a.id[:], b.id[:]) })
	out := make([]par2go.InputFile, len(items))
	for i, it := range items {
		out[i] = it.in
	}
	return out
}
