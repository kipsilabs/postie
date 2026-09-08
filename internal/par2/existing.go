package par2

import (
	"bytes"
	"context"
	"crypto/md5"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"

	"github.com/javi11/par2go"
	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/pkg/fileinfo"
)

// volumeSuffixRe matches the part of a PAR2 volume name that follows the set's
// base name, e.g. ".vol00+01.par2". Anchored on both ends so a set whose name
// merely starts with ours ("movie.mkv.1080p.vol00+01.par2") never matches.
var volumeSuffixRe = regexp.MustCompile(`^\.vol\d+\+\d+\.par2$`)

// PAR2 packet layout (all little-endian):
//
//	[8]  magic "PAR2\x00PKT"
//	[8]  packet length, header included
//	[16] packet MD5
//	[16] recovery set id
//	[16] packet type
//	[..] body
const (
	par2HeaderSize     = 64
	par2FileDescMinLen = 16 + 16 + 16 + 8 // fileID, hashFull, hash16k, length
	// maxPar2PacketLen bounds a single packet while scanning the main file so a
	// corrupt length cannot make us allocate or seek absurdly.
	maxPar2PacketLen = 1 << 30
	hash16kLen       = 16384
)

var (
	par2Magic        = [8]byte{'P', 'A', 'R', '2', 0, 'P', 'K', 'T'}
	par2TypeFileDesc = [16]byte{'P', 'A', 'R', ' ', '2', '.', '0', 0, 'F', 'i', 'l', 'e', 'D', 'e', 's', 'c'}
)

// WorkSubdir is the directory Postie creates inside par2.temp_dir for the PAR2
// files it generates. Keeping them out of the temp root lets the sweeper
// remove orphans without touching anything that is not Postie's.
const WorkSubdir = "postie-par2"

// WorkDir returns where generated PAR2 files go when temp_dir is configured,
// or "" when they are written next to the source files.
func WorkDir(cfg *config.Par2Config) string {
	if cfg == nil || cfg.TempDir == "" {
		return ""
	}
	return filepath.Join(cfg.TempDir, WorkSubdir)
}

// Result is the outcome of a PAR2 request. Created lists the files this call
// wrote, which Postie owns and may delete once they are no longer needed.
// Reused lists PAR2 files that already existed on disk (the user's own, or a
// verified leftover from an earlier run); they are posted alongside the
// sources but are never Postie's to delete.
type Result struct {
	Created []string
	Reused  []string
}

// All returns every PAR2 file to post, reused first.
func (r Result) All() []string {
	return slices.Concat(r.Reused, r.Created)
}

// fileDesc is the identity a PAR2 File Description packet records for one
// input file: its name inside the set, its length and the MD5 of its first
// 16 KiB. Together they say which bytes the set can repair.
type fileDesc struct {
	name    string
	size    uint64
	hash16k [16]byte
}

// par2SetOnDisk returns the main PAR2 file plus every volume belonging to the
// set named base in dirPath. Only "<base>.par2" and "<base>.volN+M.par2" are
// accepted; nothing else that merely shares the prefix.
func par2SetOnDisk(ctx context.Context, dirPath, base string) []string {
	main := filepath.Join(dirPath, base+".par2")
	out := []string{main}

	entries, err := os.ReadDir(dirPath)
	if err != nil {
		slog.WarnContext(ctx, "Failed to read directory for par2 volumes", "dir", dirPath, "error", err)
		return out
	}
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		suffix, ok := strings.CutPrefix(name, base)
		if !ok || !volumeSuffixRe.MatchString(suffix) {
			continue
		}
		out = append(out, filepath.Join(dirPath, name))
	}
	return out
}

// readFileDescs parses the File Description packets of the PAR2 file at path.
// The main .par2 file carries no recovery data, so this reads only metadata.
func readFileDescs(path string) ([]fileDesc, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer func() { _ = f.Close() }()

	var descs []fileDesc
	var header [par2HeaderSize]byte
	for {
		if _, err := io.ReadFull(f, header[:]); err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			return nil, fmt.Errorf("par2 %s: read packet header: %w", path, err)
		}
		if !bytes.Equal(header[:8], par2Magic[:]) {
			return nil, fmt.Errorf("par2 %s: bad packet magic", path)
		}
		length := binary.LittleEndian.Uint64(header[8:16])
		if length < par2HeaderSize || length > maxPar2PacketLen {
			return nil, fmt.Errorf("par2 %s: implausible packet length %d", path, length)
		}
		bodyLen := int64(length - par2HeaderSize)

		if !bytes.Equal(header[48:64], par2TypeFileDesc[:]) {
			if _, err := f.Seek(bodyLen, io.SeekCurrent); err != nil {
				return nil, fmt.Errorf("par2 %s: skip packet: %w", path, err)
			}
			continue
		}
		if bodyLen < par2FileDescMinLen {
			return nil, fmt.Errorf("par2 %s: FileDesc packet too short (%d bytes)", path, bodyLen)
		}
		body := make([]byte, bodyLen)
		if _, err := io.ReadFull(f, body); err != nil {
			return nil, fmt.Errorf("par2 %s: read FileDesc: %w", path, err)
		}
		var d fileDesc
		copy(d.hash16k[:], body[32:48])
		d.size = binary.LittleEndian.Uint64(body[48:56])
		d.name = string(bytes.TrimRight(body[56:], "\x00"))
		descs = append(descs, d)
	}
	if len(descs) == 0 {
		return nil, fmt.Errorf("par2 %s: no FileDesc packets", path)
	}
	return descs, nil
}

// describeSource computes the fileDesc a PAR2 creator would record for the
// file at path under the given in-set name (mirrors par2go's quick scan: MD5
// of the first 16 KiB, or of the whole file when shorter).
func describeSource(path, name string) (fileDesc, error) {
	f, err := os.Open(path)
	if err != nil {
		return fileDesc{}, err
	}
	defer func() { _ = f.Close() }()

	st, err := f.Stat()
	if err != nil {
		return fileDesc{}, err
	}
	d := fileDesc{name: name, size: uint64(st.Size())}
	if st.Size() == 0 {
		return d, nil
	}
	buf := make([]byte, min(st.Size(), hash16kLen))
	if _, err := io.ReadFull(f, buf); err != nil {
		return fileDesc{}, fmt.Errorf("read first 16k of %s: %w", path, err)
	}
	d.hash16k = md5.Sum(buf)
	return d, nil
}

// par2Describes reports whether the PAR2 set whose main file is mainPath was
// built from exactly inputs: every input must appear with the same name, size
// and leading hash, and the set must not describe anything else. A set found
// on disk that fails this test belongs to other data (an earlier upload with
// the same name, a changed file, a folder that gained or lost files) and must
// not be attached to this upload.
func par2Describes(ctx context.Context, mainPath string, inputs []par2go.InputFile) bool {
	descs, err := readFileDescs(mainPath)
	if err != nil {
		slog.WarnContext(ctx, "Ignoring unreadable PAR2 file", "path", mainPath, "error", err)
		return false
	}
	if len(descs) != len(inputs) {
		slog.WarnContext(ctx, "Ignoring PAR2 set that describes a different number of files",
			"path", mainPath, "par2Files", len(descs), "inputFiles", len(inputs))
		return false
	}

	byName := make(map[string]fileDesc, len(descs))
	for _, d := range descs {
		byName[d.name] = d
	}
	for _, in := range inputs {
		want, err := describeSource(in.Path, in.Name)
		if err != nil {
			slog.WarnContext(ctx, "Cannot fingerprint source file for PAR2 reuse", "path", in.Path, "error", err)
			return false
		}
		if got, ok := byName[in.Name]; !ok || got != want {
			slog.WarnContext(ctx, "Ignoring PAR2 set that does not describe the source file",
				"path", mainPath, "sourceFile", in.Path)
			return false
		}
	}
	return true
}

// setInputNames maps the files of a folder set to the names recorded in its
// FileDesc packets: the path relative to folderDir with forward slashes (the
// job folder a downloader creates is already named after the NZB, so the
// top-level folder name is not part of it). Falls back to the base name.
func setInputNames(inputs []fileinfo.FileInfo, folderDir string) []par2go.InputFile {
	out := make([]par2go.InputFile, len(inputs))
	for i, f := range inputs {
		name, err := filepath.Rel(folderDir, f.Path)
		if err != nil || name == "" || name == "." {
			name = filepath.Base(f.Path)
		}
		out[i] = par2go.InputFile{Path: f.Path, Name: filepath.ToSlash(name)}
	}
	return out
}
