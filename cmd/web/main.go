package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/gorilla/mux"
	"github.com/kipsilabs/postie/frontend"
	"github.com/kipsilabs/postie/internal/backend"
	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/wshub"
	"github.com/spf13/cobra"
)

// For development, serve static files from disk
// In production, these would be embedded
var frontendBuildPath = "../../frontend/build"

// ErrorResponse represents a structured error response
type ErrorResponse struct {
	Error     string `json:"error"`
	Message   string `json:"message"`
	Code      string `json:"code,omitempty"`
	Details   string `json:"details,omitempty"`
	Timestamp string `json:"timestamp"`
}

// SetupErrorCode represents different types of setup errors
type SetupErrorCode string

const (
	SetupErrorInvalidInput     SetupErrorCode = "INVALID_INPUT"
	SetupErrorServerValidation SetupErrorCode = "SERVER_VALIDATION_FAILED"
	SetupErrorFileSystem       SetupErrorCode = "FILESYSTEM_ERROR"
	SetupErrorConfigSave       SetupErrorCode = "CONFIG_SAVE_FAILED"
	SetupErrorInternal         SetupErrorCode = "INTERNAL_ERROR"
)

var (
	port string
	host string
)

// WebEventEmitter is a function that emits events for web mode
type WebEventEmitter func(eventType string, data any)

type WebServer struct {
	app          *backend.App
	router       *mux.Router
	wsHub        *wshub.Hub
	eventEmitter WebEventEmitter
}

func NewWebServer() *WebServer {
	app := backend.NewApp()
	router := mux.NewRouter()
	wsHub := wshub.NewHub()

	ws := &WebServer{
		app:    app,
		router: router,
		wsHub:  wsHub,
	}

	// Create event emitter function
	ws.eventEmitter = func(eventType string, data any) {
		ws.wsHub.EmitEvent(eventType, data)
	}

	// Set web mode and event emitter
	app.SetWebMode(true)
	app.SetWebEventEmitter(ws.eventEmitter)

	ws.setupRoutes()

	// Start WebSocket hub
	go wsHub.Run()

	return ws
}

func (ws *WebServer) setupRoutes() {
	// Request logging middleware
	ws.router.Use(func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			// CORS headers
			w.Header().Set("Access-Control-Allow-Origin", "*")
			w.Header().Set("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")
			w.Header().Set("Access-Control-Allow-Headers", "Content-Type, Authorization")

			if r.Method == "OPTIONS" {
				w.WriteHeader(http.StatusOK)
				return
			}

			next.ServeHTTP(w, r)
		})
	})

	ws.router.HandleFunc("/live", ws.LiveHandler).Methods("GET")

	// API routes - register WebSocket first with explicit path
	ws.router.HandleFunc("/api/ws", ws.handleWebSocket)

	// API routes
	api := ws.router.PathPrefix("/api").Subrouter()

	// REST API endpoints
	api.HandleFunc("/status", ws.handleGetStatus).Methods("GET")
	api.HandleFunc("/config", ws.handleGetConfig).Methods("GET")
	api.HandleFunc("/config", ws.handleSaveConfig).Methods("POST")
	api.HandleFunc("/config/pending/status", ws.handleConfigPendingStatus).Methods("GET")
	api.HandleFunc("/config/pending", ws.handlePendingConfig).Methods("GET")
	api.HandleFunc("/config/pending/apply", ws.handleApplyPendingConfig).Methods("POST")
	api.HandleFunc("/config/pending/discard", ws.handleConfigPendingDiscard).Methods("POST")
	api.HandleFunc("/queue", ws.handleGetQueueItems).Methods("GET")
	api.HandleFunc("/queue", ws.handleClearQueue).Methods("DELETE")
	api.HandleFunc("/queue/add-files", ws.handleAddFilesToQueue).Methods("POST")
	api.HandleFunc("/queue/{id}", ws.handleRemoveFromQueue).Methods("DELETE")
	api.HandleFunc("/queue/{id}/retry", ws.handleRetryJob).Methods("POST")
	api.HandleFunc("/queue/{id}/cancel", ws.handleCancelJob).Methods("DELETE")
	api.HandleFunc("/queue/{id}/priority", ws.handleSetQueueItemPriority).Methods("POST")
	api.HandleFunc("/queue/stats", ws.handleGetQueueStats).Methods("GET")
	api.HandleFunc("/logs", ws.handleGetLogs).Methods("GET")
	api.HandleFunc("/logs/download", ws.handleDownloadLogs).Methods("GET")
	api.HandleFunc("/upload", ws.handleUpload).Methods("POST")
	api.HandleFunc("/upload-folder", ws.handleUploadFolder).Methods("POST")
	api.HandleFunc("/upload/cancel", ws.handleCancelUpload).Methods("POST")
	api.HandleFunc("/nzb/{id}/download", ws.handleDownloadNZB).Methods("GET")
	api.HandleFunc("/processor/status", ws.handleGetProcessorStatus).Methods("GET")
	api.HandleFunc("/processor/pause", ws.handlePauseProcessing).Methods("POST")
	api.HandleFunc("/processor/resume", ws.handleResumeProcessing).Methods("POST")
	api.HandleFunc("/processor/paused", ws.handleIsProcessingPaused).Methods("GET")
	api.HandleFunc("/processor/auto-paused", ws.handleIsProcessingAutoPaused).Methods("GET")
	api.HandleFunc("/processor/auto-pause-reason", ws.handleGetAutoPauseReason).Methods("GET")
	api.HandleFunc("/running-jobs", ws.handleGetRunningJobs).Methods("GET")
	api.HandleFunc("/running-job-details", ws.handleGetRunningJobDetails).Methods("GET")
	api.HandleFunc("/validate-server", ws.handleValidateServer).Methods("POST")
	api.HandleFunc("/setup/complete", ws.handleSetupComplete).Methods("POST")
	api.HandleFunc("/metrics/nntp-pool", ws.handleGetNntpPoolMetrics).Methods("GET")
	api.HandleFunc("/metrics/transfer-runtime", ws.handleGetTransferRuntimeMetrics).Methods("GET")
	api.HandleFunc("/filesystem/browse", ws.handleBrowseFilesystem).Methods("GET")
	api.HandleFunc("/filesystem/import", ws.handleImportFiles).Methods("POST")
	api.HandleFunc("/watcher/status", ws.handleGetWatcherStatus).Methods("GET")
	api.HandleFunc("/watcher/scan", ws.handleTriggerScan).Methods("POST")

	// Arr webhook integration (web mode only — hidden in desktop UI)
	api.HandleFunc("/arr/instances", ws.handleListArrInstances).Methods("GET")
	api.HandleFunc("/arr/instances", ws.handleAddArrInstance).Methods("POST")
	api.HandleFunc("/arr/instances/{id}", ws.handleRemoveArrInstance).Methods("DELETE")
	api.HandleFunc("/arr/instances/{id}/test", ws.handleTestArrConnection).Methods("POST")
	// Webhook receiver — API key validated from ?apiKey query param (arr can't send custom headers)
	api.HandleFunc("/arr/webhook/{instanceId}", ws.handleArrWebhook).Methods("POST")

	// API key management for the settings UI (open routes, same as other UI APIs)
	api.HandleFunc("/api-key", ws.handleGetAPIKey).Methods("GET")
	api.HandleFunc("/api-key/regenerate", ws.handleRegenerateAPIKey).Methods("POST")

	// Gated external API: only mount when api.enabled = true and protected by
	// the X-API-Key / Bearer token middleware.
	v1 := ws.router.PathPrefix("/api/v1").Subrouter()
	v1.Use(ws.apiKeyMiddleware)
	v1.HandleFunc("/queue/upload", ws.handleAPIQueueUpload).Methods("POST")

	// Serve static files (catch-all)
	ws.router.PathPrefix("/").Handler(ws.getStaticFileHandler())
}

func (ws *WebServer) getStaticFileHandler() http.Handler {
	var fs http.FileSystem

	// Check if we should use embedded filesystem or development path
	if _, err := os.Stat(frontendBuildPath); err == nil {
		// Development mode - serve from disk
		fs = http.Dir(frontendBuildPath)
	} else {
		// Production mode - serve from embedded filesystem
		buildFS, err := frontend.GetBuildFS()
		if err != nil {
			slog.Warn("Failed to get embedded filesystem, using disk fallback", "error", err)
			fs = http.Dir(frontendBuildPath)
		} else {
			fs = http.FS(buildFS)
		}
	}

	fileServer := http.FileServer(fs)

	// SPA fallback handler with prerendered page support
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		path := r.URL.Path

		// Try to open the exact file
		if f, err := fs.Open(path); err == nil {
			_ = f.Close()
			fileServer.ServeHTTP(w, r)
			return
		}

		// For paths without extension, try .html version (SvelteKit prerendered pages)
		ext := filepath.Ext(path)
		if ext == "" {
			htmlPath := path + ".html"
			if f, err := fs.Open(htmlPath); err == nil {
				_ = f.Close()
				// Serve the prerendered .html file
				r.URL.Path = htmlPath
				fileServer.ServeHTTP(w, r)
				return
			}

			// No prerendered page exists - serve index.html as SPA fallback
			r.URL.Path = "/index.html"
			fileServer.ServeHTTP(w, r)
			return
		}

		// Asset file not found - return 404
		http.NotFound(w, r)
	})
}

// LiveHandler returns a simple response to indicate the server is live
func (ws *WebServer) LiveHandler(w http.ResponseWriter, r *http.Request) {
	w.WriteHeader(http.StatusOK)
	w.Header().Set("Content-Type", "application/json")
	response := map[string]string{"status": "live"}

	if err := json.NewEncoder(w).Encode(response); err != nil {
		slog.Error("Error encoding live response", "error", err)
		http.Error(w, "Internal Server Error", http.StatusInternalServerError)
		return
	}
}

func (ws *WebServer) handleGetWatcherStatus(w http.ResponseWriter, r *http.Request) {
	status := ws.app.GetWatcherStatus()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(status)
}

func (ws *WebServer) handleTriggerScan(w http.ResponseWriter, r *http.Request) {
	ws.app.TriggerScan()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
}

func (ws *WebServer) handleWebSocket(w http.ResponseWriter, r *http.Request) {
	ws.wsHub.ServeHTTP(w, r)
}

func (ws *WebServer) handleGetStatus(w http.ResponseWriter, r *http.Request) {
	status := ws.app.GetAppStatus()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(status)
}

func (ws *WebServer) handleGetConfig(w http.ResponseWriter, r *http.Request) {
	config, err := ws.app.GetConfig()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(config)
}

func (ws *WebServer) handleSaveConfig(w http.ResponseWriter, r *http.Request) {
	var configData config.ConfigData
	if err := json.NewDecoder(r.Body).Decode(&configData); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	if err := configData.Validate(); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	if err := ws.app.SaveConfig(&configData); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleGetQueueItems(w http.ResponseWriter, r *http.Request) {
	// Parse pagination parameters
	query := r.URL.Query()

	page, _ := strconv.Atoi(query.Get("page"))
	if page < 1 {
		page = 1
	}

	limit, _ := strconv.Atoi(query.Get("limit"))
	if limit < 1 || limit > 100 {
		limit = 25 // Default limit
	}

	sortBy := query.Get("sortBy")
	if sortBy == "" {
		sortBy = "created" // Default sort field
	}

	order := query.Get("order")
	if order != "asc" && order != "desc" {
		order = "desc" // Default sort order
	}

	status := query.Get("status")
	if status != "" && status != "pending" && status != "complete" && status != "error" && status != "running" {
		status = ""
	}

	// Create pagination parameters
	params := backend.PaginationParams{
		Page:   page,
		Limit:  limit,
		SortBy: sortBy,
		Order:  order,
		Status: status,
	}

	// Get paginated results
	result, err := ws.app.GetQueueItems(params)
	if err != nil {
		slog.Error("Failed to get paginated queue items", "error", err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(result)
}

func (ws *WebServer) handleRetryJob(w http.ResponseWriter, r *http.Request) {
	vars := mux.Vars(r)
	id := vars["id"]

	if err := ws.app.RetryJob(id); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleCancelJob(w http.ResponseWriter, r *http.Request) {
	vars := mux.Vars(r)
	id := vars["id"]

	if err := ws.app.CancelJob(id); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleConfigPendingStatus(w http.ResponseWriter, r *http.Request) {
	status := ws.app.HasPendingConfigChanges()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(status)
}

func (ws *WebServer) handleApplyPendingConfig(w http.ResponseWriter, r *http.Request) {
	if err := ws.app.ApplyPendingConfig(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleConfigPendingDiscard(w http.ResponseWriter, r *http.Request) {
	if err := ws.app.DiscardPendingConfig(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handlePendingConfig(w http.ResponseWriter, r *http.Request) {
	pendingConfig := ws.app.GetPendingConfigStatus()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(pendingConfig)
}

func (ws *WebServer) handleGetLogs(w http.ResponseWriter, r *http.Request) {
	limitStr := r.URL.Query().Get("limit")
	offsetStr := r.URL.Query().Get("offset")

	limit := 100
	offset := 0

	if limitStr != "" {
		if l, err := strconv.Atoi(limitStr); err == nil {
			limit = l
		}
	}

	if offsetStr != "" {
		if o, err := strconv.Atoi(offsetStr); err == nil {
			offset = o
		}
	}

	logs, err := ws.app.GetLogsPaginated(limit, offset)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "text/plain")
	_, _ = w.Write([]byte(logs))
}

func (ws *WebServer) handleDownloadLogs(w http.ResponseWriter, r *http.Request) {
	// Get full log content (0, 0 means get last 1MB)
	logs, err := ws.app.GetLogs()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	// Generate filename with current date
	filename := fmt.Sprintf("postie-%s.log", time.Now().Format("2006-01-02"))

	// Set headers for file download
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.Header().Set("Content-Disposition", fmt.Sprintf("attachment; filename=\"%s\"", filename))
	w.Header().Set("Content-Length", fmt.Sprintf("%d", len(logs)))

	// Write the log content
	_, _ = w.Write([]byte(logs))
}

func (ws *WebServer) handleUpload(w http.ResponseWriter, r *http.Request) {
	if err := r.ParseMultipartForm(10 << 20); err != nil { // 10 MB
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	files := r.MultipartForm.File["files"]
	if len(files) == 0 {
		http.Error(w, "No files provided", http.StatusBadRequest)
		return
	}

	slog.Info("Processing uploaded files", "count", len(files))

	// Emit initial upload progress
	ws.wsHub.EmitEvent("upload-progress", map[string]any{
		"stage":     "saving",
		"progress":  0,
		"fileCount": len(files),
	})

	// Create temporary directory for uploaded files
	tempDir, err := os.MkdirTemp("", "postie-*")
	if err != nil {
		http.Error(w, "Failed to create temporary directory", http.StatusInternalServerError)
		return
	}

	// Save uploaded files to temporary location
	var filePaths []string
	for i, fileHeader := range files {
		file, err := fileHeader.Open()
		if err != nil {
			http.Error(w, "Failed to open uploaded file", http.StatusInternalServerError)
			return
		}
		defer func() {
			_ = file.Close()
		}()

		// Create temporary file
		tempFilePath := filepath.Join(tempDir, fileHeader.Filename)
		tempFile, err := os.Create(tempFilePath)
		if err != nil {
			http.Error(w, "Failed to create temporary file", http.StatusInternalServerError)
			return
		}
		defer func() {
			_ = tempFile.Close()
		}()

		// Copy uploaded file content to temporary file
		if _, err := io.Copy(tempFile, file); err != nil {
			http.Error(w, "Failed to save uploaded file", http.StatusInternalServerError)
			return
		}

		filePaths = append(filePaths, tempFilePath)

		// Emit progress for file saving
		progress := float64(i+1) / float64(len(files)) * 50 // 50% for saving files
		ws.wsHub.EmitEvent("upload-progress", map[string]any{
			"stage":       "saving",
			"progress":    progress,
			"currentFile": fileHeader.Filename,
			"fileCount":   len(files),
		})
	}

	// Emit progress for processing stage
	ws.wsHub.EmitEvent("upload-progress", map[string]any{
		"stage":     "processing",
		"progress":  75,
		"fileCount": len(files),
	})

	if err := ws.app.HandleDroppedFiles(filePaths); err != nil {
		ws.wsHub.EmitEvent("upload-error", err.Error())
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	// Emit completion
	ws.wsHub.EmitEvent("upload-complete", map[string]any{
		"fileCount": len(files),
		"message":   "Files successfully processed and added to queue",
	})

	w.WriteHeader(http.StatusOK)
}

// handleUploadFolder handles folder uploads from the web UI
// It preserves the folder structure and processes the folder as a single NZB
func (ws *WebServer) handleUploadFolder(w http.ResponseWriter, r *http.Request) {
	// Parse multipart form with larger limit for folders (100 MB)
	if err := r.ParseMultipartForm(100 << 20); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	folderName := r.FormValue("folderName")
	if folderName == "" {
		http.Error(w, "Folder name is required", http.StatusBadRequest)
		return
	}

	files := r.MultipartForm.File["files"]
	paths := r.MultipartForm.Value["paths"]

	if len(files) == 0 {
		http.Error(w, "No files provided", http.StatusBadRequest)
		return
	}

	if len(files) != len(paths) {
		http.Error(w, "Mismatch between files and paths count", http.StatusBadRequest)
		return
	}

	slog.Info("Processing folder upload", "folder", folderName, "count", len(files))

	// Emit initial upload progress
	ws.wsHub.EmitEvent("upload-progress", map[string]any{
		"stage":      "saving",
		"progress":   0,
		"fileCount":  len(files),
		"folderName": folderName,
	})

	// Create temporary directory for the folder
	tempDir, err := os.MkdirTemp("", "postie-folder-*")
	if err != nil {
		http.Error(w, "Failed to create temporary directory", http.StatusInternalServerError)
		return
	}

	// Save uploaded files preserving directory structure
	for i, fileHeader := range files {
		relativePath := paths[i]

		file, err := fileHeader.Open()
		if err != nil {
			http.Error(w, "Failed to open uploaded file", http.StatusInternalServerError)
			return
		}

		// Create the full destination path (preserving folder structure)
		destPath := filepath.Join(tempDir, relativePath)

		// Ensure parent directories exist
		if err := os.MkdirAll(filepath.Dir(destPath), 0755); err != nil {
			_ = file.Close()
			http.Error(w, "Failed to create directory structure", http.StatusInternalServerError)
			return
		}

		// Create destination file
		destFile, err := os.Create(destPath)
		if err != nil {
			_ = file.Close()
			http.Error(w, "Failed to create file", http.StatusInternalServerError)
			return
		}

		// Copy file content
		if _, err := io.Copy(destFile, file); err != nil {
			_ = file.Close()
			_ = destFile.Close()
			http.Error(w, "Failed to save file", http.StatusInternalServerError)
			return
		}

		_ = file.Close()
		_ = destFile.Close()

		// Emit progress for file saving
		progress := float64(i+1) / float64(len(files)) * 50 // 50% for saving files
		ws.wsHub.EmitEvent("upload-progress", map[string]any{
			"stage":       "saving",
			"progress":    progress,
			"currentFile": fileHeader.Filename,
			"fileCount":   len(files),
			"folderName":  folderName,
		})
	}

	// Emit progress for processing stage
	ws.wsHub.EmitEvent("upload-progress", map[string]any{
		"stage":      "processing",
		"progress":   75,
		"fileCount":  len(files),
		"folderName": folderName,
	})

	// Call HandleDroppedFiles with the folder path (this will trigger FOLDER: prefix handling)
	folderPath := filepath.Join(tempDir, folderName)
	if err := ws.app.HandleDroppedFiles([]string{folderPath}); err != nil {
		ws.wsHub.EmitEvent("upload-error", err.Error())
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	// Emit completion
	ws.wsHub.EmitEvent("upload-complete", map[string]any{
		"fileCount":  len(files),
		"folderName": folderName,
		"message":    "Folder successfully processed and added to queue as single NZB",
	})

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleGetProcessorStatus(w http.ResponseWriter, r *http.Request) {
	status := ws.app.GetProcessorStatus()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(status)
}

func (ws *WebServer) handleGetRunningJobs(w http.ResponseWriter, r *http.Request) {
	jobs, err := ws.app.GetRunningJobs()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(jobs)
}

func (ws *WebServer) handleGetRunningJobDetails(w http.ResponseWriter, r *http.Request) {
	jobDetails, err := ws.app.GetRunningJobsDetails()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(jobDetails)
}

func (ws *WebServer) handleClearQueue(w http.ResponseWriter, r *http.Request) {
	if err := ws.app.ClearQueue(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleAddFilesToQueue(w http.ResponseWriter, r *http.Request) {
	if err := ws.app.AddFilesToQueue(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleRemoveFromQueue(w http.ResponseWriter, r *http.Request) {
	vars := mux.Vars(r)
	id := vars["id"]

	if err := ws.app.RemoveFromQueue(id); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleSetQueueItemPriority(w http.ResponseWriter, r *http.Request) {
	vars := mux.Vars(r)
	id := vars["id"]

	var requestBody struct {
		Priority int `json:"priority"`
	}
	if err := json.NewDecoder(r.Body).Decode(&requestBody); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	if err := ws.app.SetQueueItemPriority(id, requestBody.Priority); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleCancelUpload(w http.ResponseWriter, r *http.Request) {
	if err := ws.app.CancelUpload(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleDownloadNZB(w http.ResponseWriter, r *http.Request) {
	vars := mux.Vars(r)
	id := vars["id"]

	nzbContent, fileName, err := ws.app.GetNZB(id)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	// Set headers for file download
	w.Header().Set("Content-Type", "application/x-nzb")
	w.Header().Set("Content-Disposition", fmt.Sprintf("attachment; filename=\"%s\"", fileName))
	w.Header().Set("Content-Length", fmt.Sprintf("%d", len(nzbContent)))

	// Write the NZB content
	_, _ = w.Write([]byte(nzbContent))
}

func (ws *WebServer) handleGetQueueStats(w http.ResponseWriter, r *http.Request) {
	stats, err := ws.app.GetQueueStats()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(stats)
}

func (ws *WebServer) handleValidateServer(w http.ResponseWriter, r *http.Request) {
	var serverData backend.ServerData
	if err := json.NewDecoder(r.Body).Decode(&serverData); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	result := ws.app.ValidateNNTPServer(serverData)
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(result)
}

// sendErrorResponse sends a structured error response
func (ws *WebServer) sendErrorResponse(w http.ResponseWriter, statusCode int, errorCode SetupErrorCode, message string, details string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(statusCode)

	errorResp := ErrorResponse{
		Error:     string(errorCode),
		Message:   message,
		Code:      string(errorCode),
		Details:   details,
		Timestamp: time.Now().UTC().Format(time.RFC3339),
	}

	_ = json.NewEncoder(w).Encode(errorResp)
}

// categorizeSetupError determines the error category and user-friendly message
func (ws *WebServer) categorizeSetupError(err error) (SetupErrorCode, string, string) {
	errStr := err.Error()

	// Server validation errors
	if strings.Contains(errStr, "failed to connect") || strings.Contains(errStr, "connection test failed") {
		return SetupErrorServerValidation,
			"Server connection failed",
			"Please check your server credentials and network connectivity. Ensure the server is reachable and your credentials are correct."
	}

	// File system errors
	if strings.Contains(errStr, "permission denied") || strings.Contains(errStr, "read-only file system") {
		return SetupErrorFileSystem,
			"File system permission error",
			"Unable to save configuration file. Please check file permissions and ensure the application has write access to the configuration directory."
	}

	// Config save errors
	if strings.Contains(errStr, "failed to save") || strings.Contains(errStr, "error writing config") {
		return SetupErrorConfigSave,
			"Configuration save failed",
			"Failed to save configuration file. Please ensure sufficient disk space and write permissions."
	}

	// Input validation errors
	if strings.Contains(errStr, "server") && (strings.Contains(errStr, "host is required") || strings.Contains(errStr, "port must be")) {
		return SetupErrorInvalidInput,
			"Invalid server configuration",
			"Please verify all server fields are properly filled out with valid values."
	}

	if strings.Contains(errStr, "output directory") {
		return SetupErrorInvalidInput,
			"Invalid output directory",
			"Please specify a valid output directory path."
	}

	// Generic internal error
	return SetupErrorInternal,
		"Setup failed",
		"An unexpected error occurred during setup. Please try again or contact support if the problem persists."
}

func (ws *WebServer) handleSetupComplete(w http.ResponseWriter, r *http.Request) {
	slog.Info("Setup completion requested", "addr", r.RemoteAddr)

	var wizardData backend.SetupWizardData
	if err := json.NewDecoder(r.Body).Decode(&wizardData); err != nil {
		slog.Error("Setup completion failed - invalid JSON", "error", err)
		ws.sendErrorResponse(w, http.StatusBadRequest, SetupErrorInvalidInput,
			"Invalid request data",
			"The request body contains invalid JSON. Please ensure all required fields are properly formatted.")
		return
	}

	slog.Info("Processing setup wizard data", "servers", len(wizardData.Servers), "output_dir", wizardData.OutputDirectory)

	if err := ws.app.SetupWizardComplete(wizardData); err != nil {
		slog.Error("Setup wizard completion failed", "error", err)

		// Categorize the error and provide appropriate response
		errorCode, message, details := ws.categorizeSetupError(err)

		// Determine appropriate HTTP status code
		statusCode := http.StatusInternalServerError
		if errorCode == SetupErrorInvalidInput {
			statusCode = http.StatusBadRequest
		}

		ws.sendErrorResponse(w, statusCode, errorCode, message, details)
		return
	}

	slog.Info("Setup wizard completed successfully")
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	_ = json.NewEncoder(w).Encode(map[string]any{
		"success":   true,
		"message":   "Setup completed successfully",
		"timestamp": time.Now().UTC().Format(time.RFC3339),
	})
}

func (ws *WebServer) handlePauseProcessing(w http.ResponseWriter, r *http.Request) {
	if err := ws.app.PauseProcessing(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleResumeProcessing(w http.ResponseWriter, r *http.Request) {
	if err := ws.app.ResumeProcessing(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleIsProcessingPaused(w http.ResponseWriter, r *http.Request) {
	paused := ws.app.IsProcessingPaused()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]bool{"paused": paused})
}

func (ws *WebServer) handleIsProcessingAutoPaused(w http.ResponseWriter, r *http.Request) {
	autoPaused := ws.app.IsProcessingAutoPaused()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]bool{"autoPaused": autoPaused})
}

func (ws *WebServer) handleGetAutoPauseReason(w http.ResponseWriter, r *http.Request) {
	reason := ws.app.GetAutoPauseReason()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]string{"reason": reason})
}

func (ws *WebServer) handleGetNntpPoolMetrics(w http.ResponseWriter, r *http.Request) {
	metrics, err := ws.app.GetNntpPoolMetrics()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(metrics)
}

func (ws *WebServer) handleGetTransferRuntimeMetrics(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(ws.app.GetTransferRuntimeMetrics())
}

// FileSystemItem represents a file or directory in the filesystem
type FileSystemItem struct {
	Name     string           `json:"name"`
	Path     string           `json:"path"`
	IsDir    bool             `json:"isDir"`
	Size     int64            `json:"size"`
	ModTime  string           `json:"modTime"`
	Children []FileSystemItem `json:"children,omitempty"`
}

func (ws *WebServer) handleBrowseFilesystem(w http.ResponseWriter, r *http.Request) {
	targetPath := r.URL.Query().Get("path")
	if targetPath == "" {
		targetPath = "/"
	}

	// Security: Clean the path and prevent directory traversal
	targetPath = filepath.Clean(targetPath)

	// Additional security: restrict to safe directories (configure as needed)
	// For now, allow browsing from root but this could be restricted
	if !filepath.IsAbs(targetPath) {
		targetPath = filepath.Join("/", targetPath)
	}

	items, err := ws.browseDirectory(targetPath)
	if err != nil {
		slog.Error("Error browsing directory", "path", targetPath, "error", err)
		http.Error(w, fmt.Sprintf("Failed to browse directory: %v", err), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]any{
		"path":  targetPath,
		"items": items,
	})
}

func (ws *WebServer) browseDirectory(dirPath string) ([]FileSystemItem, error) {
	entries, err := os.ReadDir(dirPath)
	if err != nil {
		return nil, err
	}

	var items []FileSystemItem
	for _, entry := range entries {
		info, err := entry.Info()
		if err != nil {
			continue // Skip items we can't read
		}

		fullPath := filepath.Join(dirPath, entry.Name())
		item := FileSystemItem{
			Name:    entry.Name(),
			Path:    fullPath,
			IsDir:   entry.IsDir(),
			Size:    info.Size(),
			ModTime: info.ModTime().Format(time.RFC3339),
		}

		items = append(items, item)
	}

	return items, nil
}

func (ws *WebServer) handleImportFiles(w http.ResponseWriter, r *http.Request) {
	var requestBody struct {
		FilePaths []string `json:"filePaths"`
	}

	if err := json.NewDecoder(r.Body).Decode(&requestBody); err != nil {
		http.Error(w, "Invalid request body", http.StatusBadRequest)
		return
	}

	if len(requestBody.FilePaths) == 0 {
		http.Error(w, "No file paths provided", http.StatusBadRequest)
		return
	}

	slog.Info("Importing files from remote filesystem", "count", len(requestBody.FilePaths))

	// Emit initial import progress
	ws.wsHub.EmitEvent("import-progress", map[string]any{
		"stage":     "validating",
		"progress":  0,
		"fileCount": len(requestBody.FilePaths),
	})

	// Validate that all files exist and are accessible
	var validFiles []string
	for i, filePath := range requestBody.FilePaths {
		// Security: Clean the path and validate
		cleanPath := filepath.Clean(filePath)
		if !filepath.IsAbs(cleanPath) {
			slog.Warn("Skipping relative path", "path", filePath)
			continue
		}

		// Check if path exists and is readable
		info, err := os.Stat(cleanPath)
		if err != nil {
			slog.Warn("Skipping inaccessible path", "path", filePath, "error", err)
			continue
		}

		if info.IsDir() {
			// Pass directory path directly - HandleDroppedFiles will process it as a single NZB
			validFiles = append(validFiles, cleanPath)
			continue
		}

		validFiles = append(validFiles, cleanPath)

		// Emit validation progress
		progress := float64(i+1) / float64(len(requestBody.FilePaths)) * 50 // 50% for validation
		ws.wsHub.EmitEvent("import-progress", map[string]any{
			"stage":       "validating",
			"progress":    progress,
			"currentFile": filepath.Base(filePath),
			"fileCount":   len(requestBody.FilePaths),
		})
	}

	if len(validFiles) == 0 {
		ws.wsHub.EmitEvent("import-error", "No valid files found to import")
		http.Error(w, "No valid files found to import", http.StatusBadRequest)
		return
	}

	// Emit processing stage
	ws.wsHub.EmitEvent("import-progress", map[string]any{
		"stage":     "processing",
		"progress":  75,
		"fileCount": len(validFiles),
	})

	// Add files directly to the queue using their original paths
	if err := ws.app.HandleDroppedFiles(validFiles); err != nil {
		ws.wsHub.EmitEvent("import-error", fmt.Sprintf("Failed to process imported files: %v", err))
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	// Emit completion
	ws.wsHub.EmitEvent("import-complete", map[string]any{
		"fileCount": len(validFiles),
		"message":   fmt.Sprintf("Successfully imported %d files from remote filesystem", len(validFiles)),
	})

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]any{
		"success":       true,
		"importedCount": len(validFiles),
		"message":       fmt.Sprintf("Successfully imported %d files", len(validFiles)),
	})
}

func main() {
	Execute()
}

var rootCmd = &cobra.Command{
	Use:   "postie-web",
	Short: "Postie Web Server",
	Long: `Postie Web Server provides a web interface for uploading files.
It serves the frontend application and provides REST API endpoints for managing uploads and configuration.`,
	RunE: func(cmd *cobra.Command, args []string) error {
		server := NewWebServer()

		// Initialize the app
		ctx := context.Background()
		server.app.Startup(ctx)

		addr := fmt.Sprintf("%s:%s", host, port)
		slog.Info("Web server started", "addr", addr)

		// Create HTTP server
		httpServer := &http.Server{
			Addr:    addr,
			Handler: server.router,
		}

		// Create channel to listen for interrupt signals
		sigChan := make(chan os.Signal, 1)
		signal.Notify(sigChan, os.Interrupt, syscall.SIGTERM)

		// Start server in a goroutine
		serverErrChan := make(chan error, 1)
		go func() {
			if err := httpServer.ListenAndServe(); err != nil && err != http.ErrServerClosed {
				serverErrChan <- fmt.Errorf("failed to start web server: %w", err)
			}
		}()

		// Wait for interrupt signal or server error
		select {
		case err := <-serverErrChan:
			return err
		case <-sigChan:
			slog.Info("Received interrupt signal, shutting down gracefully")

			// Shutdown the app first
			server.app.Shutdown()

			// Create a timeout context for server shutdown
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()

			// Shutdown HTTP server
			if err := httpServer.Shutdown(shutdownCtx); err != nil {
				slog.Error("Server shutdown error", "error", err)
				return err
			}

			slog.Info("Server shut down successfully")
			return nil
		}
	},
}

func init() {
	rootCmd.PersistentFlags().StringVarP(&port, "port", "p", "8080", "Port to run the web server on")
	rootCmd.PersistentFlags().StringVar(&host, "host", "0.0.0.0", "Host address to bind the web server to")

	// Allow environment variables to override defaults
	if envPort := os.Getenv("PORT"); envPort != "" {
		port = envPort
	}
	if envHost := os.Getenv("HOST"); envHost != "" {
		host = envHost
	}
}

func Execute() {
	if err := rootCmd.Execute(); err != nil {
		slog.Error("Fatal error", "error", err)
		os.Exit(1)
	}
}
