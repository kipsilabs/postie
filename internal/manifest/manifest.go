// Package manifest provides durable, streaming, zstd-compressed JSON Lines
// manifests describing every article (segment) of a source or PAR2 file in a
// transfer. Manifests are written before network posting and are the source of
// truth for verification, re-posting and crash recovery: they let Postie reuse
// the exact Message-IDs and headers of an interrupted upload instead of
// regenerating them.
//
// On-disk format (zstd-compressed):
//
//	{"v":1,"kind":"postie-transfer-manifest"}   // header line
//	{"i":0,"src":"/data/a.mkv","mid":"<...>",...} // one ArticleRecord per line
//	{"i":1,...}
//
// Manifests are written to a temporary file and atomically renamed into place
// so a crash mid-write never leaves a partial manifest that looks complete.
package manifest

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/kipsilabs/postie/internal/article"
)

// Version is the current manifest format version. Bump when the record schema
// changes incompatibly; readers reject versions they do not understand.
const Version = 1

const manifestKind = "postie-transfer-manifest"

// FileRole identifies how a file participates in a transfer. Mirrors the
// transfer_files.file_role column.
type FileRole string

const (
	RoleOriginal      FileRole = "original"
	RoleGeneratedPar2 FileRole = "generated_par2"
	RoleExistingPar2  FileRole = "existing_par2"
)

// header is the first line of every manifest.
type header struct {
	Version int    `json:"v"`
	Kind    string `json:"kind"`
}

// ArticleRecord describes a single posted article (segment). Field tags are
// kept short to reduce manifest size for transfers with millions of articles.
type ArticleRecord struct {
	Index           int               `json:"i"`
	SourcePath      string            `json:"src"`
	FileRole        FileRole          `json:"role"`
	Offset          int64             `json:"off"`
	BodySize        uint64            `json:"size"`
	MessageID       string            `json:"mid"`
	Subject         string            `json:"subj"`
	OriginalSubject string            `json:"osubj,omitempty"`
	From            string            `json:"from"`
	Groups          []string          `json:"groups,omitempty"`
	Date            time.Time         `json:"date"`
	CustomHeaders   map[string]string `json:"hdrs,omitempty"`
	XNxgHeader      string            `json:"xnxg,omitempty"`
	FileName        string            `json:"fname"`
	PartNumber      int               `json:"part"`
	TotalParts      int               `json:"parts"`
	FileSize        int64             `json:"fsize"`
}

// RecordFromArticle builds an ArticleRecord from a posted article, capturing
// everything needed to re-post it byte-for-byte with the same Message-ID.
func RecordFromArticle(idx int, sourcePath string, role FileRole, a *article.Article) ArticleRecord {
	return ArticleRecord{
		Index:           idx,
		SourcePath:      sourcePath,
		FileRole:        role,
		Offset:          a.Offset,
		BodySize:        a.Size,
		MessageID:       a.MessageID,
		Subject:         a.Subject,
		OriginalSubject: a.OriginalSubject,
		From:            a.From,
		Groups:          a.Groups,
		Date:            a.Date,
		CustomHeaders:   a.CustomHeaders,
		XNxgHeader:      a.XNxgHeader,
		FileName:        a.FileName,
		PartNumber:      a.PartNumber,
		TotalParts:      a.TotalParts,
		FileSize:        a.FileSize,
	}
}

// FilePath returns the on-disk manifest path for a transfer file under baseDir,
// laid out as <baseDir>/<transferID>/<fileID>.jsonl.zst.
func FilePath(baseDir, transferID, fileID string) string {
	return filepath.Join(baseDir, transferID, fileID+".jsonl.zst")
}

// Writer streams ArticleRecords to a temporary file and atomically commits them
// to the final path on Commit. A Writer must be either committed or aborted;
// abandoning one leaks the temporary file.
type Writer struct {
	finalPath string
	tmpPath   string
	f         *os.File
	zw        *zstd.Encoder
	enc       *json.Encoder
	count     int
	done      bool
}

// NewWriter creates the parent directory and a temporary manifest file, writes
// the header, and returns a Writer ready to accept records.
func NewWriter(path string) (*Writer, error) {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return nil, fmt.Errorf("creating manifest dir: %w", err)
	}

	tmpPath := path + ".tmp"
	f, err := os.OpenFile(tmpPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
	if err != nil {
		return nil, fmt.Errorf("creating manifest temp file: %w", err)
	}

	zw, err := zstd.NewWriter(f)
	if err != nil {
		_ = f.Close()
		_ = os.Remove(tmpPath)
		return nil, fmt.Errorf("creating zstd writer: %w", err)
	}

	w := &Writer{
		finalPath: path,
		tmpPath:   tmpPath,
		f:         f,
		zw:        zw,
		enc:       json.NewEncoder(zw),
	}

	if err := w.enc.Encode(header{Version: Version, Kind: manifestKind}); err != nil {
		_ = w.Abort()
		return nil, fmt.Errorf("writing manifest header: %w", err)
	}

	return w, nil
}

// Write appends one article record to the manifest.
func (w *Writer) Write(rec ArticleRecord) error {
	if w.done {
		return errors.New("manifest writer already closed")
	}
	if err := w.enc.Encode(rec); err != nil {
		return fmt.Errorf("encoding manifest record: %w", err)
	}
	w.count++
	return nil
}

// Count returns the number of article records written so far.
func (w *Writer) Count() int { return w.count }

// Commit flushes and fsyncs the manifest, then atomically renames it into
// place. After Commit the Writer is closed.
func (w *Writer) Commit() error {
	if w.done {
		return errors.New("manifest writer already closed")
	}
	w.done = true

	if err := w.zw.Close(); err != nil {
		_ = w.f.Close()
		_ = os.Remove(w.tmpPath)
		return fmt.Errorf("closing zstd writer: %w", err)
	}
	if err := w.f.Sync(); err != nil {
		_ = w.f.Close()
		_ = os.Remove(w.tmpPath)
		return fmt.Errorf("syncing manifest: %w", err)
	}
	if err := w.f.Close(); err != nil {
		_ = os.Remove(w.tmpPath)
		return fmt.Errorf("closing manifest: %w", err)
	}
	if err := os.Rename(w.tmpPath, w.finalPath); err != nil {
		_ = os.Remove(w.tmpPath)
		return fmt.Errorf("committing manifest: %w", err)
	}
	return nil
}

// Abort closes and removes the temporary file without committing. Safe to call
// after Commit (no-op).
func (w *Writer) Abort() error {
	if w.done {
		return nil
	}
	w.done = true
	_ = w.zw.Close()
	_ = w.f.Close()
	if err := os.Remove(w.tmpPath); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}

// Reader streams ArticleRecords from a committed manifest. It validates the
// header on open and never loads the whole manifest into memory.
type Reader struct {
	f       *os.File
	zr      *zstd.Decoder
	dec     *json.Decoder
	version int
}

// OpenReader opens a manifest, validates its header, and returns a Reader
// positioned at the first article record.
func OpenReader(path string) (*Reader, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}

	zr, err := zstd.NewReader(f)
	if err != nil {
		_ = f.Close()
		return nil, fmt.Errorf("creating zstd reader: %w", err)
	}

	dec := json.NewDecoder(zr)

	var h header
	if err := dec.Decode(&h); err != nil {
		zr.Close()
		_ = f.Close()
		return nil, fmt.Errorf("reading manifest header: %w", err)
	}
	if h.Kind != manifestKind {
		zr.Close()
		_ = f.Close()
		return nil, fmt.Errorf("not a postie transfer manifest (kind=%q)", h.Kind)
	}
	if h.Version > Version {
		zr.Close()
		_ = f.Close()
		return nil, fmt.Errorf("manifest version %d is newer than supported %d", h.Version, Version)
	}

	return &Reader{f: f, zr: zr, dec: dec, version: h.Version}, nil
}

// Version returns the manifest format version.
func (r *Reader) Version() int { return r.version }

// Next returns the next article record, or io.EOF when the manifest is
// exhausted.
func (r *Reader) Next() (ArticleRecord, error) {
	var rec ArticleRecord
	if !r.dec.More() {
		return rec, io.EOF
	}
	if err := r.dec.Decode(&rec); err != nil {
		return rec, err
	}
	return rec, nil
}

// Close releases the underlying reader and file.
func (r *Reader) Close() error {
	r.zr.Close()
	return r.f.Close()
}
