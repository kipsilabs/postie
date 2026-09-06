package backend

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/kipsilabs/postie/internal/apikey"
	"github.com/kipsilabs/postie/internal/queue"
)

// errAPINotInitialized is returned when the database is not yet ready.
var errAPINotInitialized = errors.New("api key store not initialized: database is unavailable")

// apiKeyStore returns a fresh apikey.Store bound to the current database
// connection, or an error if the database has not been initialized yet.
func (a *App) apiKeyStore() (apikey.Store, error) {
	if a.database == nil || a.database.DB == nil {
		return nil, errAPINotInitialized
	}
	return apikey.NewSQLStore(a.database.DB), nil
}

// GetAPIKey returns the persisted HTTP API key, generating one on first call
// when none exists. Exposed to the desktop UI via Wails.
func (a *App) GetAPIKey() (string, error) {
	store, err := a.apiKeyStore()
	if err != nil {
		return "", err
	}
	return apikey.EnsureKey(a.ctx, store)
}

// RegenerateAPIKey replaces the stored key with a fresh one and returns the
// new value. Exposed to the desktop UI via Wails.
func (a *App) RegenerateAPIKey() (string, error) {
	store, err := a.apiKeyStore()
	if err != nil {
		return "", err
	}
	return apikey.Regenerate(a.ctx, store)
}

// IsAPIEnabled reports whether the gated HTTP API endpoint is currently enabled
// in config.
func (a *App) IsAPIEnabled() bool {
	if a.config == nil {
		return false
	}
	return a.config.GetAPIConfig().Enabled
}

// APIQueueUploadRequest is the JSON body accepted by the gated upload endpoint.
type APIQueueUploadRequest struct {
	File              string `json:"file"`
	RelativePath      string `json:"relative_path"`
	Priority          int    `json:"priority,omitempty"`
	DeleteAfterUpload bool   `json:"delete_after_upload,omitempty"`
}

// APIQueueUploadResult describes the side-effect of a successful enqueue call.
type APIQueueUploadResult struct {
	Status string `json:"status"`
	File   string `json:"file"`
}

// EnqueueAPIUpload validates an API upload request and pushes it onto the queue
// with the appropriate per-job options. Used by the HTTP handler in cmd/web.
func (a *App) EnqueueAPIUpload(ctx context.Context, req APIQueueUploadRequest) (*APIQueueUploadResult, error) {
	if a.queue == nil {
		return nil, errors.New("queue not initialized")
	}
	if req.File == "" {
		return nil, errors.New("file is required")
	}
	if req.RelativePath == "" {
		return nil, errors.New("relative_path is required")
	}

	cleanFile := filepath.Clean(req.File)
	cleanRoot := filepath.Clean(req.RelativePath)
	if !filepath.IsAbs(cleanFile) {
		return nil, fmt.Errorf("file must be an absolute path: %q", req.File)
	}
	if !filepath.IsAbs(cleanRoot) {
		return nil, fmt.Errorf("relative_path must be an absolute path: %q", req.RelativePath)
	}
	if cleanRoot == cleanFile || !strings.HasPrefix(cleanFile, cleanRoot+string(os.PathSeparator)) {
		return nil, fmt.Errorf("relative_path %q is not a parent directory of file %q", req.RelativePath, req.File)
	}

	info, err := os.Stat(cleanFile)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("file not found: %s", cleanFile)
		}
		return nil, fmt.Errorf("stat file: %w", err)
	}
	if !info.Mode().IsRegular() {
		return nil, fmt.Errorf("file must be a regular file: %s", cleanFile)
	}

	delete := req.DeleteAfterUpload
	opts := queue.AddOptions{
		Priority:       req.Priority,
		InputFolder:    cleanRoot,
		DeleteOriginal: &delete,
	}
	if err := a.queue.AddFileWithOptions(ctx, cleanFile, info.Size(), opts); err != nil {
		return nil, fmt.Errorf("enqueue file: %w", err)
	}

	return &APIQueueUploadResult{Status: "queued", File: cleanFile}, nil
}
