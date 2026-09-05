//go:build e2e

package e2e_test

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"os/exec"
	"runtime"
	"testing"
	"time"

	"github.com/chromedp/chromedp"
)

// getConfig fetches the full config from GET /api/config.
func getConfig(t *testing.T) map[string]any {
	t.Helper()
	return mustGetRawConfig(t)
}

// saveConfig posts the full config to POST /api/config.
func saveConfig(t *testing.T, cfg map[string]any) {
	t.Helper()
	body, err := json.Marshal(cfg)
	if err != nil {
		t.Fatalf("marshal config: %v", err)
	}
	resp, err := http.Post(baseURL+"/api/config", "application/json", bytes.NewReader(body))
	if err != nil {
		t.Fatalf("POST /api/config: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("POST /api/config returned %d", resp.StatusCode)
	}
}

// patchConfig is a read-modify-write helper: it fetches the current config,
// merges patch into the named top-level section, and saves.
func patchConfig(t *testing.T, section string, patch map[string]any) {
	t.Helper()
	cfg := getConfig(t)

	sec, ok := cfg[section].(map[string]any)
	if !ok {
		sec = make(map[string]any)
	}
	for k, v := range patch {
		sec[k] = v
	}
	cfg[section] = sec

	saveConfig(t, cfg)
}

// patchWatcherConfig patches the first element of the "watchers" array.
func patchWatcherConfig(t *testing.T, patch map[string]any) {
	t.Helper()
	cfg := getConfig(t)

	watchers, _ := cfg["watchers"].([]any)
	if len(watchers) == 0 {
		watchers = []any{make(map[string]any)}
	}
	watcher, _ := watchers[0].(map[string]any)
	if watcher == nil {
		watcher = make(map[string]any)
	}
	for k, v := range patch {
		watcher[k] = v
	}
	watchers[0] = watcher
	cfg["watchers"] = watchers

	saveConfig(t, cfg)
}

// requireChrome skips the test if no Chrome/Chromium executable is found.
func requireChrome(t *testing.T) {
	t.Helper()
	candidates := []string{"google-chrome", "chromium", "chromium-browser", "google-chrome-stable"}
	for _, name := range candidates {
		if _, err := exec.LookPath(name); err == nil {
			return
		}
	}
	// macOS: check /Applications
	if runtime.GOOS == "darwin" {
		paths := []string{
			"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
			"/Applications/Chromium.app/Contents/MacOS/Chromium",
		}
		for _, p := range paths {
			if _, err := exec.LookPath(p); err == nil {
				return
			}
		}
	}
	t.Skip("Chrome/Chromium not found — skipping UI test")
}

// newChromedpCtx creates a headless Chrome context for UI tests.
func newChromedpCtx(t *testing.T) (context.Context, context.CancelFunc) {
	t.Helper()
	requireChrome(t)

	opts := append(chromedp.DefaultExecAllocatorOptions[:],
		chromedp.Flag("headless", true),
		chromedp.NoSandbox,
		chromedp.Flag("disable-gpu", true),
		// Workaround for Chromium ThreadCache crash on newer Linux kernels
		// (FATAL:scheduler_loop_quarantine_support.h Check failed: ThreadCache::IsValid)
		chromedp.Flag("disable-features", "PartitionAlloc"),
		// A cold Chrome start on a loaded CI runner can exceed chromedp's
		// default 20s wait for the DevTools websocket URL, which surfaces as
		// "websocket url timeout reached" before the page is even opened.
		chromedp.WSURLReadTimeout(60*time.Second),
	)
	allocCtx, allocCancel := chromedp.NewExecAllocator(context.Background(), opts...)
	ctx, cancel := chromedp.NewContext(allocCtx)

	ctx, timeoutCancel := context.WithTimeout(ctx, 90*time.Second)

	return ctx, func() {
		timeoutCancel()
		cancel()
		allocCancel()
	}
}

// openSettingsTab navigates to /settings and clicks the named tab.
// tabLabel must match the aria-label of the DaisyUI radio tab input.
func openSettingsTab(_ context.Context, tabLabel string) chromedp.Tasks {
	tabSelector := `input[role="tab"][aria-label="` + tabLabel + `"]`
	// Wait for the tab radio itself to appear — this confirms the settings page
	// has fully hydrated and the Svelte config fetch has completed, so any
	// {#if enabled} blocks inside the tab panel are already rendered.
	return chromedp.Tasks{
		chromedp.Navigate(baseURL + "/settings"),
		chromedp.WaitVisible(tabSelector, chromedp.ByQuery),
		chromedp.Click(tabSelector, chromedp.ByQuery),
		chromedp.Sleep(200 * time.Millisecond),
	}
}
