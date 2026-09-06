package backend

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"

	"github.com/kipsilabs/postie/internal/watcher"
)

func (a *App) initializeWatchers() error {
	defer a.recoverPanic("initializeWatchers")

	if a.config == nil {
		return fmt.Errorf("config not loaded")
	}

	// Stop previous watchers if running
	if a.watchCancel != nil {
		a.watchCancel()
		a.watchCancel = nil
	}
	for _, w := range a.watchers {
		_ = w.Close()
	}
	a.watchers = nil

	watcherCfgs := a.config.GetWatcherConfigs()

	// Check dependencies
	if a.queue == nil {
		slog.Warn("Queue not available, skipping watcher initialization")
		return nil
	}

	// Create parent context for all watchers
	a.watchCtx, a.watchCancel = context.WithCancel(context.Background())

	anyEnabled := false
	for i, watcherCfg := range watcherCfgs {
		if !watcherCfg.Enabled {
			slog.Info("Watcher disabled in configuration", "index", i, "name", watcherCfg.Name)
			continue
		}

		anyEnabled = true

		// Get watch directory from config, or use default if not set
		watchDir := watcherCfg.WatchDirectory
		if watchDir == "" {
			// Use the OS-specific data directory for watch folder as default
			watchDir = filepath.Join(a.appPaths.Data, "watch")
		}

		// Ensure watch directory exists
		if _, err := os.Stat(watchDir); os.IsNotExist(err) {
			if err := os.MkdirAll(watchDir, 0755); err != nil {
				slog.Error("Failed to create watch directory", "watchDir", watchDir, "error", err)
				continue
			}
		} else if err != nil {
			slog.Error("Failed to check watch directory", "watchDir", watchDir, "error", err)
			continue
		}

		w := watcher.New(watcherCfg, a.queue, a.processor, watchDir)
		a.watchers = append(a.watchers, w)

		// Start watcher in its own goroutine using shared parent context
		go func(w *watcher.Watcher, dir string) {
			if err := w.Start(a.watchCtx); err != nil && err != context.Canceled {
				slog.Error("Watcher error", "watchDir", dir, "error", err)
			}
		}(w, watchDir)

		slog.Info("Watcher initialized successfully", "watchDir", watchDir, "name", watcherCfg.Name)
	}

	if !anyEnabled {
		slog.Info("No watchers enabled in configuration")
	}

	return nil
}

// TriggerScan triggers an immediate directory scan on all active watchers
func (a *App) TriggerScan() {
	defer a.recoverPanic("TriggerScan")

	if len(a.watchers) == 0 {
		slog.Warn("TriggerScan called but no watchers are initialized")
		return
	}

	for _, w := range a.watchers {
		w.TriggerScan(a.watchCtx)
	}
}

// GetWatcherStatus returns the current status of all watchers
func (a *App) GetWatcherStatus() []watcher.WatcherStatusInfo {
	defer a.recoverPanic("GetWatcherStatus")

	if a.config == nil {
		return []watcher.WatcherStatusInfo{}
	}

	watcherCfgs := a.config.GetWatcherConfigs()
	result := make([]watcher.WatcherStatusInfo, 0, len(watcherCfgs))

	// Build a map from watchDir to running watcher for lookup
	runningByDir := make(map[string]*watcher.Watcher)
	for _, w := range a.watchers {
		status := w.GetWatcherStatus()
		runningByDir[status.WatchDirectory] = w
	}

	for i, cfg := range watcherCfgs {
		if !cfg.Enabled {
			// Return basic status for disabled watchers
			result = append(result, watcher.WatcherStatusInfo{
				Name:             cfg.Name,
				Enabled:          false,
				Initialized:      false,
				WatchDirectory:   cfg.WatchDirectory,
				CheckInterval:    string(cfg.CheckInterval),
				IsWithinSchedule: false,
			})
			continue
		}

		// Get actual watch dir (may differ from config if config has empty dir)
		watchDir := cfg.WatchDirectory
		if watchDir == "" {
			watchDir = filepath.Join(a.appPaths.Data, "watch")
		}

		// Check if there's a running watcher for this directory
		if w, ok := runningByDir[watchDir]; ok {
			status := w.GetWatcherStatus()
			// Ensure name from config is used (in case watcher was created with old config)
			if status.Name == "" {
				status.Name = cfg.Name
			}
			result = append(result, status)
			continue
		}

		// Watcher is enabled but not running
		status := watcher.WatcherStatusInfo{
			Name:             cfg.Name,
			Enabled:          true,
			Initialized:      false,
			WatchDirectory:   watchDir,
			CheckInterval:    string(cfg.CheckInterval),
			IsWithinSchedule: true,
		}
		if cfg.Schedule.StartTime != "" && cfg.Schedule.EndTime != "" {
			status.Schedule = &watcher.WatcherScheduleInfo{
				StartTime: cfg.Schedule.StartTime,
				EndTime:   cfg.Schedule.EndTime,
			}
		}
		_ = i
		result = append(result, status)
	}

	return result
}
