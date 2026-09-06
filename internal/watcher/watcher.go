package watcher

import (
	"context"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/queue"
)

// WatcherScheduleInfo represents the schedule configuration
type WatcherScheduleInfo struct {
	StartTime string `json:"start_time"`
	EndTime   string `json:"end_time"`
}

// WatcherStatusInfo represents the watcher status information
type WatcherStatusInfo struct {
	Name             string               `json:"name"`
	Enabled          bool                 `json:"enabled"`
	Initialized      bool                 `json:"initialized"`
	WatchDirectory   string               `json:"watch_directory"`
	CheckInterval    string               `json:"check_interval"`
	NextRun          string               `json:"next_run,omitempty"`
	IsWithinSchedule bool                 `json:"is_within_schedule"`
	Schedule         *WatcherScheduleInfo `json:"schedule,omitempty"`
	Error            string               `json:"error,omitempty"`
}

// ProcessorInterface defines the interface for checking running jobs
type ProcessorInterface interface {
	IsPathBeingProcessed(path string) bool
}

type Watcher struct {
	cfg           config.WatcherConfig
	queue         queue.QueueInterface
	processor     ProcessorInterface
	watchFolder   string
	fileSizeCache map[string]fileCacheEntry
	cacheMutex    sync.RWMutex
	nextRunTime   time.Time
	nextRunMutex  sync.RWMutex
}

type fileCacheEntry struct {
	size      int64
	timestamp time.Time
}

func New(
	cfg config.WatcherConfig,
	q queue.QueueInterface,
	processor ProcessorInterface,
	watchFolder string,
) *Watcher {
	return &Watcher{
		cfg:           cfg,
		queue:         q,
		processor:     processor,
		watchFolder:   watchFolder,
		fileSizeCache: make(map[string]fileCacheEntry),
	}
}

// isSymlink checks if the given path is a symbolic link using Lstat.
// Returns true if the path is a symlink, false otherwise.
func isSymlink(path string) (bool, error) {
	fileInfo, err := os.Lstat(path) // Lstat doesn't follow symlinks
	if err != nil {
		return false, err
	}
	return fileInfo.Mode()&os.ModeSymlink != 0, nil
}

func (w *Watcher) Start(ctx context.Context) error {
	slog.InfoContext(ctx, fmt.Sprintf("Starting directory watching %s with interval %v", w.watchFolder, w.cfg.CheckInterval))

	scanTicker := time.NewTicker(w.cfg.CheckInterval.ToDuration())
	defer scanTicker.Stop()

	// Cache cleanup ticker (runs every hour)
	cacheCleanupTicker := time.NewTicker(1 * time.Hour)
	defer cacheCleanupTicker.Stop()

	// Initialize next run time
	w.updateNextRunTime()

	// Start continuous directory scanning
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-scanTicker.C:
			w.updateNextRunTime() // Update next run time before checking schedule
			if w.isWithinSchedule() {
				if err := w.scanDirectory(ctx); err != nil {
					slog.ErrorContext(ctx, "Error scanning directory", "error", err)
				}
			} else {
				slog.Info("Not within schedule, skipping scan")
			}
		case <-cacheCleanupTicker.C:
			w.cleanupOldCacheEntries()
		}
	}
}

func (w *Watcher) isWithinSchedule() bool {
	if w.cfg.Schedule.StartTime == "" || w.cfg.Schedule.EndTime == "" {
		return true
	}

	now := time.Now()
	currentTime := now.Format("15:04")

	startTime := w.cfg.Schedule.StartTime
	endTime := w.cfg.Schedule.EndTime

	// Handle crossing midnight
	if startTime <= endTime {
		return currentTime >= startTime && currentTime <= endTime
	} else {
		return currentTime >= startTime || currentTime <= endTime
	}
}

func (w *Watcher) scanDirectory(ctx context.Context) error {
	// First, calculate total directory size
	totalSize, err := w.calculateDirectorySize(ctx)
	if err != nil {
		return fmt.Errorf("failed to calculate directory size: %w", err)
	}

	slog.InfoContext(ctx, "Directory size check", "directory", w.watchFolder, "total_size", totalSize, "threshold", w.cfg.SizeThreshold)

	// Check if directory size meets threshold
	if totalSize < w.cfg.SizeThreshold {
		slog.DebugContext(ctx, "Directory size below threshold, skipping import", "total_size", totalSize, "threshold", w.cfg.SizeThreshold)
		return nil
	}

	slog.InfoContext(ctx, "Directory size exceeds threshold, starting import", "total_size", totalSize, "threshold", w.cfg.SizeThreshold)

	// If single NZB per folder mode is enabled, collect files by folder
	if w.cfg.SingleNzbPerFolder {
		return w.scanDirectoryGroupByFolder(ctx)
	}

	// Process all files in directory (traditional mode - one NZB per file)
	return w.walkFiles(ctx, func(path string, info os.FileInfo) error {

		slog.DebugContext(ctx, "Processing file", "path", path, "size", info.Size(), "mod_time", info.ModTime())

		// Skip files that don't meet criteria
		if !w.shouldProcessFile(path, info) {
			slog.DebugContext(ctx, "File does not meet criteria, skipping", "path", path)

			return nil
		}

		// Check if file is currently being processed
		if w.processor != nil && w.processor.IsPathBeingProcessed(path) {
			slog.InfoContext(ctx, "File is currently being processed, ignoring", "path", path)
			return nil
		}

		// Check if file is already in queue (pending, completed, or errored)
		inQueue, err := w.queue.IsPathInQueue(path)
		if err != nil {
			slog.ErrorContext(ctx, "Error checking if path is in queue", "path", path, "error", err)
			return nil // Continue processing other files
		}

		if inQueue {
			slog.DebugContext(ctx, "File already exists in queue, ignoring", "path", path)
			return nil
		}

		// Add file to queue with this watcher's root so the processor derives the
		// relative output path from the correct watch folder (issue #168: only the
		// first configured watcher's root was known to the processor).
		err = w.queue.AddFileWithOptions(ctx, path, info.Size(), queue.AddOptions{InputFolder: w.watchFolder})

		if err != nil {
			slog.ErrorContext(ctx, "Error adding file to queue", "path", path, "error", err)
			return nil // Continue processing other files
		}

		slog.InfoContext(ctx, "Added file to queue", "path", filepath.Base(path), "size", info.Size())

		return nil
	})
}

// scanDirectoryGroupByFolder scans the directory and groups files by folder for single NZB per folder mode
func (w *Watcher) scanDirectoryGroupByFolder(ctx context.Context) error {
	// Map to collect files by their top-level subdirectory under the watch folder
	filesByFolder := make(map[string][]string)
	sizeByFolder := make(map[string]int64)
	individualFiles := make(map[string]int64)
	var mapMutex sync.Mutex

	// Walk the directory tree and collect files by folder
	err := w.walkFiles(ctx, func(path string, info os.FileInfo) error {

		// Skip files that don't meet criteria
		if !w.shouldProcessFile(path, info) {
			return nil
		}

		// Determine which top-level subdirectory this file belongs to
		relPath, err := filepath.Rel(w.watchFolder, path)
		if err != nil {
			return err
		}
		parts := strings.SplitN(filepath.ToSlash(relPath), "/", 2)

		mapMutex.Lock()
		if len(parts) == 1 {
			// File is directly in the watch folder — handle individually
			individualFiles[path] = info.Size()
		} else {
			// File is inside a subdirectory — group under the top-level subdir
			folderPath := filepath.Join(w.watchFolder, parts[0])
			filesByFolder[folderPath] = append(filesByFolder[folderPath], path)
			sizeByFolder[folderPath] += info.Size()
		}
		mapMutex.Unlock()
		return nil
	})

	if err != nil {
		return err
	}

	// Process each folder as a unit
	for folderPath, files := range filesByFolder {
		if len(files) == 0 {
			continue
		}

		// First check if the folder itself is already in queue or being processed (with FOLDER: prefix)
		folderQueuePath := "FOLDER:" + folderPath

		// Check if folder is currently being processed
		if w.processor != nil && w.processor.IsPathBeingProcessed(folderQueuePath) {
			slog.InfoContext(ctx, "Folder is currently being processed, skipping", "folder", folderPath)
			continue
		}

		// Check if folder is already in queue
		inQueue, err := w.queue.IsPathInQueue(folderQueuePath)
		if err != nil {
			slog.ErrorContext(ctx, "Error checking if folder is in queue", "folder", folderPath, "error", err)
		} else if inQueue {
			slog.DebugContext(ctx, "Folder already in queue, skipping", "folder", folderPath)
			continue
		}

		// Check if any file in the folder is being processed or in queue
		skipFolder := false
		for _, filePath := range files {
			// Check if file is currently being processed
			if w.processor != nil && w.processor.IsPathBeingProcessed(filePath) {
				slog.InfoContext(ctx, "File in folder is currently being processed, skipping folder", "file", filePath, "folder", folderPath)
				skipFolder = true
				break
			}

			// Check if file is already in queue
			inQueue, err := w.queue.IsPathInQueue(filePath)
			if err != nil {
				slog.ErrorContext(ctx, "Error checking if path is in queue", "path", filePath, "error", err)
				continue
			}

			if inQueue {
				slog.DebugContext(ctx, "File in folder already exists in queue, skipping folder", "file", filePath, "folder", folderPath)
				skipFolder = true
				break
			}
		}

		if skipFolder {
			continue
		}

		// Use the folder path as the queue identifier
		// This allows the processor to know which files belong to this folder job
		folderSize := sizeByFolder[folderPath]
		folderName := filepath.Base(folderPath)

		slog.InfoContext(ctx, "Adding folder to queue", "folder", folderName, "files", len(files), "size", folderSize)

		// Add the folder to the queue with a special marker to indicate it's a folder
		// folderQueuePath was already computed above with "FOLDER:" + folderPath prefix
		err = w.queue.AddFileWithOptions(ctx, folderQueuePath, folderSize, queue.AddOptions{InputFolder: w.watchFolder})
		if err != nil {
			slog.ErrorContext(ctx, "Error adding folder to queue", "folder", folderPath, "error", err)
			continue
		}

		// Store the file list for this folder so the processor can retrieve them
		// This would typically be stored in the queue metadata or a separate storage
		// For now, we'll log them
		for _, filePath := range files {
			slog.DebugContext(ctx, "File in folder", "folder", folderName, "file", filepath.Base(filePath))
		}
	}

	// Process files that are directly in the watch folder (not in any subdirectory)
	for path, size := range individualFiles {
		// Check if file is currently being processed
		if w.processor != nil && w.processor.IsPathBeingProcessed(path) {
			slog.InfoContext(ctx, "File is currently being processed, ignoring", "path", path)
			continue
		}

		// Check if file is already in queue
		inQueue, err := w.queue.IsPathInQueue(path)
		if err != nil {
			slog.ErrorContext(ctx, "Error checking if path is in queue", "path", path, "error", err)
			continue
		}

		if inQueue {
			slog.DebugContext(ctx, "File already exists in queue, ignoring", "path", path)
			continue
		}

		if err := w.queue.AddFileWithOptions(ctx, path, size, queue.AddOptions{InputFolder: w.watchFolder}); err != nil {
			slog.ErrorContext(ctx, "Error adding file to queue", "path", path, "error", err)
			continue
		}

		slog.InfoContext(ctx, "Added file to queue", "path", filepath.Base(path), "size", size)
	}

	return nil
}

// walkFiles calls fn for every regular file under the watch folder, skipping
// directories and (unless configured otherwise) symlinks. An entry that cannot
// be read or stat'ed is logged and skipped rather than aborting the whole scan:
// a single permission-denied subfolder, or a file removed between listing and
// stat, must not stop every other file from being imported. Only an unreadable
// watch folder root is an error.
func (w *Watcher) walkFiles(ctx context.Context, fn func(path string, info os.FileInfo) error) error {
	return filepath.WalkDir(w.watchFolder, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			if path == w.watchFolder {
				return err
			}
			slog.WarnContext(ctx, "Skipping unreadable entry in watch folder", "path", path, "error", err)
			if entry != nil && entry.IsDir() {
				return fs.SkipDir
			}
			return nil
		}

		if entry.IsDir() {
			return nil
		}

		if !w.cfg.FollowSymlinks {
			isSymlinkFile, err := isSymlink(path)
			if err != nil {
				slog.DebugContext(ctx, "Error checking if path is symlink, skipping", "path", path, "error", err)
				return nil
			}
			if isSymlinkFile {
				slog.DebugContext(ctx, "Skipping symlink", "path", path)
				return nil
			}
		}

		info, err := entry.Info()
		if err != nil {
			slog.DebugContext(ctx, "Skipping entry that could not be stat'ed", "path", path, "error", err)
			return nil
		}

		return fn(path, info)
	})
}

func (w *Watcher) shouldProcessFile(path string, info os.FileInfo) bool {
	// Check minimum file size
	if info.Size() < int64(w.cfg.MinFileSize) {
		return false
	}

	// Check ignore patterns
	for _, pattern := range w.cfg.IgnorePatterns {
		matched, err := filepath.Match(pattern, filepath.Base(path))
		if err != nil {
			slog.Warn("Invalid pattern", "pattern", pattern, "error", err)
			continue
		}
		if matched {
			return false
		}
	}

	// Check if file is stable (not being written to)
	if !w.isFileStable(path, info) {
		return false
	}

	return true
}

// calculateDirectorySize calculates the total size of all files in the watch directory
func (w *Watcher) calculateDirectorySize(ctx context.Context) (int64, error) {
	var totalSize int64

	err := w.walkFiles(ctx, func(path string, info os.FileInfo) error {

		// Only count files that meet basic criteria (not ignore patterns and min file size)
		if w.shouldCountFile(path, info) {
			atomic.AddInt64(&totalSize, info.Size())
		}

		return nil
	})

	return totalSize, err
}

// shouldCountFile checks if a file should be counted towards directory size
func (w *Watcher) shouldCountFile(path string, info os.FileInfo) bool {
	// Check minimum file size
	if info.Size() < int64(w.cfg.MinFileSize) {
		return false
	}

	// Check ignore patterns
	for _, pattern := range w.cfg.IgnorePatterns {
		matched, err := filepath.Match(pattern, filepath.Base(path))
		if err != nil {
			slog.Warn("Invalid pattern", "pattern", pattern, "error", err)
			continue
		}
		if matched {
			return false
		}
	}

	return true
}

// isFileStable checks if a file is stable (not being written to) using multiple methods
func (w *Watcher) isFileStable(path string, info os.FileInfo) bool {
	// Method 1: Check if file modification time is older than MinFileAge
	minAge := w.cfg.MinFileAge.ToDuration()
	if minAge == 0 {
		minAge = 60 * time.Second // fallback if not configured
	}
	if time.Since(info.ModTime()) < minAge {
		return false
	}

	// Method 2: Check file size stability by comparing current size with cached size
	// This detects files that are still growing
	if !w.isFileSizeStable(path, info.Size()) {
		return false
	}

	return true
}

// isFileSizeStable checks if file size has remained constant
func (w *Watcher) isFileSizeStable(path string, currentSize int64) bool {
	w.cacheMutex.Lock()
	defer w.cacheMutex.Unlock()

	cachedEntry, exists := w.fileSizeCache[path]
	w.fileSizeCache[path] = fileCacheEntry{
		size:      currentSize,
		timestamp: time.Now(),
	}

	// If this is the first time we see this file, it's not stable yet
	if !exists {
		return false
	}

	// If size changed, file is not stable
	if cachedEntry.size != currentSize {
		return false
	}

	return true
}

// cleanupOldCacheEntries removes cache entries older than 24 hours to prevent memory leaks
func (w *Watcher) cleanupOldCacheEntries() {
	w.cacheMutex.Lock()
	defer w.cacheMutex.Unlock()

	cutoff := time.Now().Add(-24 * time.Hour)
	for path, entry := range w.fileSizeCache {
		if entry.timestamp.Before(cutoff) {
			delete(w.fileSizeCache, path)
		}
	}
}

// TriggerScan triggers an immediate directory scan
func (w *Watcher) TriggerScan(ctx context.Context) {
	go func() {
		if w.isWithinSchedule() {
			slog.InfoContext(ctx, "Watch scanning started", "watch_directory", w.watchFolder)

			if err := w.scanDirectory(ctx); err != nil {
				slog.ErrorContext(ctx, "Error in triggered directory scan", "error", err)
			}
		}
	}()
}

// GetQueueItems returns queue items via the queue
func (w *Watcher) GetQueueItems(params queue.PaginationParams) (*queue.PaginatedResult, error) {
	return w.queue.GetQueueItems(params)
}

// RemoveFromQueue removes an item from the queue via queue
func (w *Watcher) RemoveFromQueue(id string) error {
	return w.queue.RemoveFromQueue(id)
}

// ClearQueue removes all completed and failed items from the queue via queue
func (w *Watcher) ClearQueue() error {
	return w.queue.ClearQueue()
}

// GetQueueStats returns statistics about the queue via queue
func (w *Watcher) GetQueueStats() (map[string]any, error) {
	return w.queue.GetQueueStats()
}

// updateNextRunTime calculates and sets the next scheduled run time
func (w *Watcher) updateNextRunTime() {
	w.nextRunMutex.Lock()
	defer w.nextRunMutex.Unlock()

	now := time.Now()
	interval := w.cfg.CheckInterval.ToDuration()

	// Calculate next run based on current time plus interval
	nextRun := now.Add(interval)

	// If schedule is configured, check if the next run would be within schedule
	if w.cfg.Schedule.StartTime != "" && w.cfg.Schedule.EndTime != "" {
		// If the next run would be outside schedule, calculate when it would next be in schedule
		if !w.wouldBeWithinSchedule(nextRun) {
			nextRun = w.getNextScheduledRun(now)
		}
	}

	w.nextRunTime = nextRun
}

// GetNextRunTime returns the next scheduled run time
func (w *Watcher) GetNextRunTime() time.Time {
	w.nextRunMutex.RLock()
	defer w.nextRunMutex.RUnlock()
	return w.nextRunTime
}

// wouldBeWithinSchedule checks if a given time would be within the configured schedule
func (w *Watcher) wouldBeWithinSchedule(t time.Time) bool {
	if w.cfg.Schedule.StartTime == "" || w.cfg.Schedule.EndTime == "" {
		return true
	}

	timeStr := t.Format("15:04")
	startTime := w.cfg.Schedule.StartTime
	endTime := w.cfg.Schedule.EndTime

	// Handle crossing midnight
	if startTime <= endTime {
		return timeStr >= startTime && timeStr <= endTime
	} else {
		return timeStr >= startTime || timeStr <= endTime
	}
}

// getNextScheduledRun calculates when the next run should be within the schedule
func (w *Watcher) getNextScheduledRun(now time.Time) time.Time {
	startTime := w.cfg.Schedule.StartTime
	endTime := w.cfg.Schedule.EndTime

	// Parse start time for today
	startHour := 0
	startMin := 0
	if len(startTime) >= 5 {
		_, _ = fmt.Sscanf(startTime, "%d:%d", &startHour, &startMin)
	}

	// Calculate start time for today
	todayStart := time.Date(now.Year(), now.Month(), now.Day(), startHour, startMin, 0, 0, now.Location())

	// If start time today has passed and we're crossing midnight, use tomorrow's start time
	currentTime := now.Format("15:04")
	if startTime <= endTime && currentTime > endTime {
		// Schedule ends today, next run is tomorrow at start time
		return todayStart.Add(24 * time.Hour)
	} else if startTime > endTime && currentTime > endTime && currentTime < startTime {
		// We're in the gap between end and start time (same day)
		return todayStart
	} else if todayStart.Before(now) && startTime <= endTime {
		// Start time today has passed, next run is tomorrow
		return todayStart.Add(24 * time.Hour)
	} else {
		// Next run is at today's start time
		return todayStart
	}
}

// GetWatcherStatus returns comprehensive watcher status information
func (w *Watcher) GetWatcherStatus() WatcherStatusInfo {
	status := WatcherStatusInfo{
		Name:             w.cfg.Name,
		Enabled:          w.cfg.Enabled,
		Initialized:      true, // If this method is called, the watcher is initialized
		WatchDirectory:   w.watchFolder,
		CheckInterval:    string(w.cfg.CheckInterval),
		IsWithinSchedule: w.isWithinSchedule(),
	}

	if w.cfg.Enabled {
		status.NextRun = w.GetNextRunTime().Format(time.RFC3339)

		if w.cfg.Schedule.StartTime != "" && w.cfg.Schedule.EndTime != "" {
			status.Schedule = &WatcherScheduleInfo{
				StartTime: w.cfg.Schedule.StartTime,
				EndTime:   w.cfg.Schedule.EndTime,
			}
		}
	}

	return status
}

// Close does nothing for the simple watcher (queue is managed separately)
func (w *Watcher) Close() error {
	return nil
}
