// Package transferwriter records a durable manifest and a transfer_files row
// for each file of a transfer before its articles are posted. It bridges the
// poster (which builds articles with their Message-IDs) to the manifest store
// and transfer persistence, so an interrupted upload can be resumed and
// verified using the exact Message-IDs and headers that were posted.
package transferwriter

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"os"
	"time"

	"github.com/kipsilabs/postie/internal/article"
	"github.com/kipsilabs/postie/internal/manifest"
	"github.com/kipsilabs/postie/internal/par2"
	"github.com/kipsilabs/postie/internal/transferstore"
)

// Recorder writes manifests + transfer_files rows for one transfer. It is
// created per job (bound to a transfer_id) and shares the process-wide store.
type Recorder struct {
	transferID string
	baseDir    string
	store      *transferstore.Store
}

// New creates a Recorder for transferID that writes manifests under baseDir and
// persists rows through store.
func New(transferID, baseDir string, store *transferstore.Store) *Recorder {
	return &Recorder{transferID: transferID, baseDir: baseDir, store: store}
}

// fileID derives a stable identifier for a source path so re-recording the same
// file (e.g. after a retry or crash) maps to the same manifest and row.
func fileID(sourcePath string) string {
	sum := sha256.Sum256([]byte(sourcePath))
	return hex.EncodeToString(sum[:8])
}

// roleFor classifies a file by its on-disk path.
func roleFor(sourcePath string) manifest.FileRole {
	if par2.IsPar2File(sourcePath) {
		return manifest.RoleGeneratedPar2
	}
	return manifest.RoleOriginal
}

// RecordFile writes an immutable manifest of articles for sourcePath (via a
// temp file + atomic rename) and upserts the corresponding transfer_files row
// in the planned state. It must be called before the file's articles are
// posted so the manifest is durable first.
func (r *Recorder) RecordFile(ctx context.Context, sourcePath string, articles []*article.Article) error {
	fid := fileID(sourcePath)
	role := roleFor(sourcePath)
	manifestPath := manifest.FilePath(r.baseDir, r.transferID, fid)

	w, err := manifest.NewWriter(manifestPath)
	if err != nil {
		return err
	}
	for i, a := range articles {
		if err := w.Write(manifest.RecordFromArticle(i, sourcePath, role, a)); err != nil {
			_ = w.Abort()
			return err
		}
	}
	if err := w.Commit(); err != nil {
		return err
	}

	return r.store.UpsertFile(ctx, transferstore.TransferFile{
		TransferID:        r.transferID,
		FileID:            fid,
		ManifestPath:      manifestPath,
		ManifestVersion:   manifest.Version,
		SourcePath:        sourcePath,
		FileRole:          string(role),
		ArticleCount:      len(articles),
		UploadState:       transferstore.StatePlanned,
		VerificationState: transferstore.StatePlanned,
	})
}

// ExistingArticles returns the article records of a previously written manifest
// for sourcePath, if one exists. On crash recovery this lets the poster reuse
// the exact Message-IDs/offsets that were already planned (and possibly partly
// posted) instead of regenerating them, which would create duplicate NZB
// segments. ok is false when no manifest exists yet (a fresh upload).
func (r *Recorder) ExistingArticles(ctx context.Context, sourcePath string) ([]manifest.ArticleRecord, bool, error) {
	path := manifest.FilePath(r.baseDir, r.transferID, fileID(sourcePath))
	if _, err := os.Stat(path); err != nil {
		if os.IsNotExist(err) {
			return nil, false, nil
		}
		return nil, false, err
	}

	reader, err := manifest.OpenReader(path)
	if err != nil {
		return nil, false, err
	}
	defer func() { _ = reader.Close() }()

	var recs []manifest.ArticleRecord
	for {
		if err := ctx.Err(); err != nil {
			return nil, false, err
		}
		rec, err := reader.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return nil, false, err
		}
		recs = append(recs, rec)
	}
	return recs, len(recs) > 0, nil
}

// CompleteUpload marks every file of this transfer as uploaded, recording
// postedAt and the first verification-check due time (nextCheckAt). It is
// called after all of the transfer's articles have been posted, so the durable
// verification service can pick the files up. The queue upload slot can be
// released once this returns; propagation delay is then borne by the background
// service, not the queue.
func (r *Recorder) CompleteUpload(ctx context.Context, postedAt, nextCheckAt time.Time, deleteOriginal bool) error {
	files, err := r.store.ListFilesByTransfer(ctx, r.transferID)
	if err != nil {
		return err
	}
	for _, f := range files {
		if err := r.store.MarkUploaded(ctx, r.transferID, f.FileID, postedAt, nextCheckAt); err != nil {
			return err
		}
		// Persist the delete-original intent on original files so the
		// post-verification cleanup can act on it later, in a different process
		// context (e.g. after a restart). PAR2 cleanup is governed by the
		// maintain_par2 config at cleanup time, not a per-file policy.
		if deleteOriginal && f.FileRole == string(manifest.RoleOriginal) {
			if err := r.store.SetCleanupPolicy(ctx, r.transferID, f.FileID, transferstore.CleanupDeleteOriginal); err != nil {
				return err
			}
		}
	}
	return nil
}
