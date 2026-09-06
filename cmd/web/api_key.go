package main

import (
	"crypto/subtle"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"strings"

	"github.com/kipsilabs/postie/internal/backend"
)

// handleGetAPIKey returns (or generates) the current API key. Open route used
// by the settings UI; the gated routes use the key via apiKeyMiddleware.
func (ws *WebServer) handleGetAPIKey(w http.ResponseWriter, r *http.Request) {
	key, err := ws.app.GetAPIKey()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"key": key})
}

// handleRegenerateAPIKey rotates the stored key and returns the new value.
func (ws *WebServer) handleRegenerateAPIKey(w http.ResponseWriter, r *http.Request) {
	key, err := ws.app.RegenerateAPIKey()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"key": key})
}

// apiKeyMiddleware authenticates callers of /api/v1/* routes via X-API-Key
// header (preferred) or Authorization: Bearer <token>. Returns 403 when the
// API is disabled in config, 401 on missing/invalid keys.
func (ws *WebServer) apiKeyMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !ws.app.IsAPIEnabled() {
			http.Error(w, "api disabled", http.StatusForbidden)
			return
		}
		expected, err := ws.app.GetAPIKey()
		if err != nil || expected == "" {
			http.Error(w, "api key not available", http.StatusInternalServerError)
			return
		}

		got := r.Header.Get("X-API-Key")
		if got == "" {
			if auth := r.Header.Get("Authorization"); strings.HasPrefix(auth, "Bearer ") {
				got = strings.TrimPrefix(auth, "Bearer ")
			}
		}
		if got == "" {
			http.Error(w, "missing api key", http.StatusUnauthorized)
			return
		}
		if subtle.ConstantTimeCompare([]byte(got), []byte(expected)) != 1 {
			http.Error(w, "invalid api key", http.StatusUnauthorized)
			return
		}
		next.ServeHTTP(w, r)
	})
}

// handleAPIQueueUpload pushes a file onto the upload queue with an explicit
// input-root prefix so the produced NZB preserves the source directory tree.
func (ws *WebServer) handleAPIQueueUpload(w http.ResponseWriter, r *http.Request) {
	var req backend.APIQueueUploadRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "invalid request body: "+err.Error(), http.StatusBadRequest)
		return
	}

	result, err := ws.app.EnqueueAPIUpload(r.Context(), req)
	if err != nil {
		// Map well-known validation messages to 4xx; everything else is 500.
		msg := err.Error()
		switch {
		case strings.Contains(msg, "file not found"):
			http.Error(w, msg, http.StatusNotFound)
		case strings.HasPrefix(msg, "file is required"),
			strings.HasPrefix(msg, "relative_path is required"),
			strings.HasPrefix(msg, "file must be"),
			strings.HasPrefix(msg, "relative_path must be"),
			strings.HasPrefix(msg, "relative_path "):
			http.Error(w, msg, http.StatusBadRequest)
		default:
			slog.Warn("api upload enqueue failed", "error", err)
			if errors.Is(err, errAPINotInitializedSentinel) {
				http.Error(w, msg, http.StatusServiceUnavailable)
				return
			}
			http.Error(w, msg, http.StatusInternalServerError)
		}
		return
	}
	writeJSON(w, http.StatusAccepted, result)
}

// errAPINotInitializedSentinel matches the backend error used when DB is not
// up yet. Kept in this package to avoid leaking internal error types.
var errAPINotInitializedSentinel = errors.New("api key store not initialized: database is unavailable")

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}
