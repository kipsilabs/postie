// Package transfercleaner performs post-verification cleanup for a durable
// transfer: once every file of a transfer has been verified, it runs the
// optional post-upload script, deletes originals whose policy requests it,
// removes generated PAR2 files (unless maintained), and removes the manifests.
//
// Safety rules:
//   - Cleanup runs ONLY when all files of the transfer reached a terminal state
//     and NONE failed verification.
//   - If any file's verification failed, nothing is deleted — every recovery
//     artifact (originals, PAR2, manifests, rows) is retained for the operator.
//   - Cleanup is idempotent: removing an already-removed file is not an error.
package transfercleaner

import (
	"context"
	"log/slog"
	"os"

	"github.com/kipsilabs/postie/internal/manifest"
	"github.com/kipsilabs/postie/internal/transferstore"
)

// ScriptRunner runs the post-upload script for a verified transfer. It may be
// nil (no script).
type ScriptRunner func(ctx context.Context, transferID string, files []transferstore.TransferFile) error

// Cleaner removes recovery artifacts after a transfer is fully verified.
type Cleaner struct {
	store        *transferstore.Store
	maintainPar2 bool
	runScript    ScriptRunner
	removeFile   func(string) error
}

// New creates a Cleaner. When maintainPar2 is true, generated PAR2 files are
// kept after verification. runScript may be nil.
func New(store *transferstore.Store, maintainPar2 bool, runScript ScriptRunner) *Cleaner {
	return &Cleaner{
		store:        store,
		maintainPar2: maintainPar2,
		runScript:    runScript,
		removeFile:   os.Remove,
	}
}

// CleanupTransfer cleans up a transfer if and only if every file is verified.
// It returns done=true only when cleanup actually ran to completion. It is a
// no-op (done=false) while any file is still pending/verifying, and it retains
// everything (done=false) if any file failed verification.
func (c *Cleaner) CleanupTransfer(ctx context.Context, transferID string) (bool, error) {
	files, err := c.store.ListFilesByTransfer(ctx, transferID)
	if err != nil {
		return false, err
	}
	if len(files) == 0 {
		return false, nil
	}

	for _, f := range files {
		switch f.VerificationState {
		case transferstore.StateVerified:
			// ready
		case transferstore.StateVerificationFailed:
			// Final failure: retain ALL recovery artifacts for the operator.
			slog.WarnContext(ctx, "Transfer has a failed file; retaining all recovery data",
				"transfer", transferID, "file", f.FileID)
			return false, nil
		default:
			// Still uploading/verifying — not ready to clean.
			return false, nil
		}
	}

	// All files verified — run the post-upload script first (best effort), then
	// delete sources/manifests.
	if c.runScript != nil {
		if err := c.runScript(ctx, transferID, files); err != nil {
			slog.WarnContext(ctx, "Post-upload script failed during cleanup", "transfer", transferID, "error", err)
		}
	}

	for _, f := range files {
		switch f.FileRole {
		case string(manifest.RoleOriginal):
			if f.CleanupPolicy == transferstore.CleanupDeleteOriginal {
				c.remove(ctx, f.SourcePath, "original")
			}
		case string(manifest.RoleGeneratedPar2):
			if !c.maintainPar2 {
				c.remove(ctx, f.SourcePath, "generated par2")
			}
		}
		// Manifests are postie's own recovery files; always removed once verified.
		c.remove(ctx, f.ManifestPath, "manifest")
	}

	// Drop the now-cleaned rows so the table stays bounded and cleanup is not
	// retried for this transfer.
	if err := c.store.DeleteFilesByTransfer(ctx, transferID); err != nil {
		return false, err
	}

	slog.InfoContext(ctx, "Transfer verified and cleaned up", "transfer", transferID, "files", len(files))
	return true, nil
}

// remove deletes a path, tolerating already-removed files (idempotent).
func (c *Cleaner) remove(ctx context.Context, path, kind string) {
	if path == "" {
		return
	}
	if err := c.removeFile(path); err != nil && !os.IsNotExist(err) {
		slog.WarnContext(ctx, "Failed to remove file during cleanup", "kind", kind, "path", path, "error", err)
	}
}
