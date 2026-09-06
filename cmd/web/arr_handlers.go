package main

import (
	"crypto/subtle"
	"encoding/json"
	"fmt"
	"net/http"
	"path/filepath"
	"strings"

	"github.com/google/uuid"
	"github.com/gorilla/mux"
	"github.com/kipsilabs/postie/internal/arr"
	"github.com/kipsilabs/postie/internal/backend"
	"github.com/kipsilabs/postie/internal/config"
)

type addArrInstanceRequest struct {
	Instance   config.ArrInstance `json:"instance"`
	WebhookURL string             `json:"webhook_url"` // e.g. "http://postie:8080"
}

func (ws *WebServer) handleListArrInstances(w http.ResponseWriter, r *http.Request) {
	instances, err := ws.app.GetArrInstances()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(instances)
}

func (ws *WebServer) handleAddArrInstance(w http.ResponseWriter, r *http.Request) {
	var req addArrInstanceRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	if req.Instance.ID == "" {
		req.Instance.ID = uuid.NewString()
	}

	apiKey, err := ws.app.GetAPIKey()
	if err != nil {
		http.Error(w, "could not retrieve API key: "+err.Error(), http.StatusInternalServerError)
		return
	}

	webhookURL := fmt.Sprintf("%s/api/arr/webhook/%s?apiKey=%s",
		strings.TrimRight(req.WebhookURL, "/"),
		req.Instance.ID,
		apiKey,
	)

	saved, err := ws.app.SetupArrWebhook(r.Context(), req.Instance, webhookURL)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusCreated)
	_ = json.NewEncoder(w).Encode(saved)
}

func (ws *WebServer) handleRemoveArrInstance(w http.ResponseWriter, r *http.Request) {
	id := mux.Vars(r)["id"]
	if err := ws.app.RemoveArrInstance(r.Context(), id); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (ws *WebServer) handleTestArrConnection(w http.ResponseWriter, r *http.Request) {
	var instance config.ArrInstance
	if err := json.NewDecoder(r.Body).Decode(&instance); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	if err := ws.app.TestArrConnection(r.Context(), instance); err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	w.WriteHeader(http.StatusOK)
}

func (ws *WebServer) handleArrWebhook(w http.ResponseWriter, r *http.Request) {
	// Validate API key from query param — arr webhooks cannot send custom headers.
	apiKey, err := ws.app.GetAPIKey()
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	incoming := r.URL.Query().Get("apiKey")
	if subtle.ConstantTimeCompare([]byte(incoming), []byte(apiKey)) != 1 {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}

	instanceID := mux.Vars(r)["instanceId"]

	var payload arr.WebhookPayload
	if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	paths := arr.ExtractFilePaths(payload)
	if len(paths) == 0 {
		// Test ping or unsupported event — acknowledge without queuing.
		w.WriteHeader(http.StatusOK)
		return
	}

	instances, err := ws.app.GetArrInstances()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	var instance *config.ArrInstance
	for i := range instances {
		if instances[i].ID == instanceID {
			instance = &instances[i]
			break
		}
	}
	if instance == nil {
		http.Error(w, "arr instance not found", http.StatusNotFound)
		return
	}

	for _, path := range paths {
		req := backend.APIQueueUploadRequest{
			File:              path,
			RelativePath:      filepath.Dir(path),
			Priority:          0,
			DeleteAfterUpload: instance.DeleteAfterUpload,
		}
		if _, err := ws.app.EnqueueAPIUpload(r.Context(), req); err != nil {
			http.Error(w, fmt.Sprintf("queuing %s: %v", path, err), http.StatusInternalServerError)
			return
		}
	}

	w.WriteHeader(http.StatusOK)
}
