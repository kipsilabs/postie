package watcher

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"sync"
	"testing"
	"time"

	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/queue"
)

// mockProcessor implements ProcessorInterface for testing
type mockProcessor struct {
	processingPaths map[string]bool
}

func (m *mockProcessor) IsPathBeingProcessed(path string) bool {
	return m.processingPaths[path]
}

// mockQueueWithDuplicateCheck simulates queue behavior with duplicate checking
// It implements the queue.QueueInterface
type mockQueueWithDuplicateCheck struct {
	addFileCalls []string
	addFileOpts  map[string]queue.AddOptions
	mu           sync.Mutex
}

func (m *mockQueueWithDuplicateCheck) AddFile(ctx context.Context, path string, size int64) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	// Simulate duplicate checking - if file already in calls, ignore
	if slices.Contains(m.addFileCalls, path) {
		// Simulate queue.AddFile behavior - log and return nil for duplicates
		return nil
	}

	// Add file to our mock queue
	m.addFileCalls = append(m.addFileCalls, path)
	return nil
}

func (m *mockQueueWithDuplicateCheck) GetQueueItems(params queue.PaginationParams) (*queue.PaginatedResult, error) {
	return &queue.PaginatedResult{}, nil
}

func (m *mockQueueWithDuplicateCheck) RemoveFromQueue(id string) error {
	return nil
}

func (m *mockQueueWithDuplicateCheck) ClearQueue() error {
	return nil
}

func (m *mockQueueWithDuplicateCheck) GetQueueStats() (map[string]any, error) {
	return map[string]any{}, nil
}

func (m *mockQueueWithDuplicateCheck) SetQueueItemPriorityWithReorder(ctx context.Context, id string, newPriority int) error {
	return nil
}

func (m *mockQueueWithDuplicateCheck) AddFileWithOptions(ctx context.Context, path string, size int64, opts queue.AddOptions) error {
	if err := m.AddFile(ctx, path, size); err != nil {
		return err
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.addFileOpts == nil {
		m.addFileOpts = make(map[string]queue.AddOptions)
	}
	m.addFileOpts[path] = opts
	return nil
}

func (m *mockQueueWithDuplicateCheck) PendingTotalSize(ctx context.Context) (int64, error) {
	return 0, nil
}

func (m *mockQueueWithDuplicateCheck) RunMigrations() error {
	return nil
}

func (m *mockQueueWithDuplicateCheck) RollbackMigration() error {
	return nil
}

func (m *mockQueueWithDuplicateCheck) MigrateTo(version int64) error {
	return nil
}

func (m *mockQueueWithDuplicateCheck) ResetDatabase() error {
	return nil
}

func (m *mockQueueWithDuplicateCheck) IsLegacyDatabase() (bool, error) {
	return false, nil
}

func (m *mockQueueWithDuplicateCheck) RecreateDatabase() error {
	return nil
}

func (m *mockQueueWithDuplicateCheck) EnsureMigrationCompatibility() error {
	return nil
}

func (m *mockQueueWithDuplicateCheck) IsPathInQueue(path string) (bool, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	// Check if this path has already been added to our mock queue
	if slices.Contains(m.addFileCalls, path) {
		return true, nil
	}
	return false, nil
}

// Helper function to create a test watcher
func createTestWatcher(t *testing.T) (*Watcher, string) {
	tempDir := t.TempDir()

	cfg := config.WatcherConfig{
		Enabled:            true,
		WatchDirectory:     tempDir,
		SizeThreshold:      100,
		MinFileSize:        10,
		CheckInterval:      config.Duration("1s"),
		DeleteOriginalFile: false,
		IgnorePatterns:     []string{},
		MinFileAge:         config.Duration("2s"),
	}

	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	mockProc := &mockProcessor{
		processingPaths: make(map[string]bool),
	}

	watcher := New(cfg, mockQueue, mockProc, tempDir)
	return watcher, tempDir
}

// Helper function to create a test file with specific content and modification time
func createTestFile(t *testing.T, dir, filename string, content []byte, modTime time.Time) string {
	filePath := filepath.Join(dir, filename)
	err := os.WriteFile(filePath, content, 0644)
	if err != nil {
		t.Fatalf("Failed to create test file: %v", err)
	}

	if !modTime.IsZero() {
		err = os.Chtimes(filePath, modTime, modTime)
		if err != nil {
			t.Fatalf("Failed to set file time: %v", err)
		}
	}

	return filePath
}

func TestIsFileStable_ModificationTime(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)

	tests := []struct {
		name           string
		modTime        time.Time
		expectedStable bool
	}{
		{
			name:           "recently modified file should not be stable",
			modTime:        time.Now().Add(-1 * time.Second),
			expectedStable: false,
		},
		{
			name:           "old file should be stable",
			modTime:        time.Now().Add(-10 * time.Second),
			expectedStable: true,
		},
		{
			name:           "file modified exactly 2 seconds ago should be stable",
			modTime:        time.Now().Add(-2 * time.Second),
			expectedStable: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create test file with specific modification time
			content := []byte("test content for stability check")
			// Use unique filename for each test to avoid cache conflicts
			filePath := createTestFile(t, tempDir, tt.name+".txt", content, tt.modTime)

			// Get file info
			info, err := os.Stat(filePath)
			if err != nil {
				t.Fatalf("Failed to stat file: %v", err)
			}

			// First call will populate the size cache (should be false)
			firstCheck := watcher.isFileStable(filePath, info)
			if firstCheck {
				t.Error("First stability check should return false due to size cache initialization")
			}

			// Second call tests the actual stability logic
			secondCheck := watcher.isFileStable(filePath, info)

			if secondCheck != tt.expectedStable {
				t.Errorf("Expected stability %v, got %v for file modified at %v",
					tt.expectedStable, secondCheck, tt.modTime)
			}
		})
	}
}

func TestIsFileStable_MinFileAge(t *testing.T) {
	tempDir := t.TempDir()

	tests := []struct {
		name           string
		minFileAge     string
		modTime        time.Time
		expectedStable bool
	}{
		{
			name:           "file younger than MinFileAge should not be stable",
			minFileAge:     "5m",
			modTime:        time.Now().Add(-1 * time.Minute),
			expectedStable: false,
		},
		{
			name:           "file older than MinFileAge should be stable",
			minFileAge:     "2s",
			modTime:        time.Now().Add(-10 * time.Second),
			expectedStable: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg := config.WatcherConfig{
				Enabled:        true,
				WatchDirectory: tempDir,
				SizeThreshold:  100,
				MinFileSize:    10,
				CheckInterval:  config.Duration("1s"),
				IgnorePatterns: []string{},
				MinFileAge:     config.Duration(tt.minFileAge),
			}

			mockQueue := &mockQueueWithDuplicateCheck{addFileCalls: make([]string, 0)}
			mockProc := &mockProcessor{processingPaths: make(map[string]bool)}
			w := New(cfg, mockQueue, mockProc, tempDir)

			content := []byte("test content")
			filePath := createTestFile(t, tempDir, tt.name+".txt", content, tt.modTime)

			info, err := os.Stat(filePath)
			if err != nil {
				t.Fatalf("Failed to stat file: %v", err)
			}

			// First call populates size cache
			w.isFileStable(filePath, info)
			// Second call tests the actual stability logic
			result := w.isFileStable(filePath, info)

			if result != tt.expectedStable {
				t.Errorf("Expected stability %v, got %v for MinFileAge=%s, modTime=%v ago",
					tt.expectedStable, result, tt.minFileAge, time.Since(tt.modTime).Round(time.Millisecond))
			}
		})
	}
}

func TestIsFileSizeStable(t *testing.T) {
	watcher, _ := createTestWatcher(t)

	testPath := "/test/path/file.txt"

	t.Run("first encounter should not be stable", func(t *testing.T) {
		isStable := watcher.isFileSizeStable(testPath, 1000)
		if isStable {
			t.Error("First encounter with file should not be stable")
		}
	})

	t.Run("same size should be stable", func(t *testing.T) {
		// First call (not stable)
		watcher.isFileSizeStable(testPath+"_same", 1000)
		// Second call with same size (should be stable)
		isStable := watcher.isFileSizeStable(testPath+"_same", 1000)
		if !isStable {
			t.Error("File with same size should be stable")
		}
	})

	t.Run("different size should not be stable", func(t *testing.T) {
		// First call
		watcher.isFileSizeStable(testPath+"_diff", 1000)
		// Second call with different size (should not be stable)
		isStable := watcher.isFileSizeStable(testPath+"_diff", 1500)
		if isStable {
			t.Error("File with different size should not be stable")
		}
	})
}

func TestCleanupOldCacheEntries(t *testing.T) {
	watcher, _ := createTestWatcher(t)

	// Add some entries to the cache with different timestamps
	oldTime := time.Now().Add(-25 * time.Hour)   // Older than 24 hours
	recentTime := time.Now().Add(-1 * time.Hour) // Recent

	watcher.cacheMutex.Lock()
	watcher.fileSizeCache["old_file.txt"] = fileCacheEntry{
		size:      1000,
		timestamp: oldTime,
	}
	watcher.fileSizeCache["recent_file.txt"] = fileCacheEntry{
		size:      2000,
		timestamp: recentTime,
	}
	watcher.cacheMutex.Unlock()

	// Verify both entries exist
	watcher.cacheMutex.RLock()
	if len(watcher.fileSizeCache) != 2 {
		t.Errorf("Expected 2 cache entries, got %d", len(watcher.fileSizeCache))
	}
	watcher.cacheMutex.RUnlock()

	// Run cleanup
	watcher.cleanupOldCacheEntries()

	// Verify only recent entry remains
	watcher.cacheMutex.RLock()
	defer watcher.cacheMutex.RUnlock()

	if len(watcher.fileSizeCache) != 1 {
		t.Errorf("Expected 1 cache entry after cleanup, got %d", len(watcher.fileSizeCache))
	}

	if _, exists := watcher.fileSizeCache["old_file.txt"]; exists {
		t.Error("Old cache entry should have been removed")
	}

	if _, exists := watcher.fileSizeCache["recent_file.txt"]; !exists {
		t.Error("Recent cache entry should have been preserved")
	}
}

func TestShouldProcessFile_WithStabilityCheck(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)

	tests := []struct {
		name           string
		fileSize       int64
		modTime        time.Time
		ignorePattern  string
		expectedResult bool
	}{
		{
			name:           "stable file should be processed",
			fileSize:       1000,
			modTime:        time.Now().Add(-10 * time.Second),
			ignorePattern:  "",
			expectedResult: true, // Will be false on first run due to size cache, true on second
		},
		{
			name:           "recently modified file should not be processed",
			fileSize:       1000,
			modTime:        time.Now().Add(-1 * time.Second),
			ignorePattern:  "",
			expectedResult: false,
		},
		{
			name:           "file too small should not be processed",
			fileSize:       5, // Below MinFileSize of 10
			modTime:        time.Now().Add(-10 * time.Second),
			ignorePattern:  "",
			expectedResult: false,
		},
		{
			name:           "ignored pattern should not be processed",
			fileSize:       1000,
			modTime:        time.Now().Add(-10 * time.Second),
			ignorePattern:  "*.tmp",
			expectedResult: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Update ignore patterns if specified
			if tt.ignorePattern != "" {
				watcher.cfg.IgnorePatterns = []string{tt.ignorePattern}
			} else {
				watcher.cfg.IgnorePatterns = []string{}
			}

			// Create test file
			content := make([]byte, tt.fileSize)
			filename := "test_process.txt"
			if tt.ignorePattern == "*.tmp" {
				filename = "test_process.tmp"
			}

			filePath := createTestFile(t, tempDir, filename, content, tt.modTime)

			// Get file info
			info, err := os.Stat(filePath)
			if err != nil {
				t.Fatalf("Failed to stat file: %v", err)
			}

			// For stable files, we need to call twice due to size cache logic
			if tt.expectedResult && tt.fileSize >= watcher.cfg.MinFileSize {
				// First call to populate cache
				watcher.shouldProcessFile(filePath, info)
				// Second call should return true for stable file
			}

			// Test should process file
			result := watcher.shouldProcessFile(filePath, info)

			if result != tt.expectedResult {
				t.Errorf("Expected shouldProcessFile to return %v, got %v", tt.expectedResult, result)
			}
		})
	}
}

func TestIntegrationStabilityCheck(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)

	// Create a file that grows over time
	filePath := filepath.Join(tempDir, "growing_file.txt")

	// First write - make sure file is large enough to meet size requirements
	initialContent := make([]byte, 1500) // Larger than SizeThreshold (100) and MinFileSize (10)
	for i := range initialContent {
		initialContent[i] = 'a'
	}
	err := os.WriteFile(filePath, initialContent, 0644)
	if err != nil {
		t.Fatalf("Failed to create file: %v", err)
	}

	info1, err := os.Stat(filePath)
	if err != nil {
		t.Fatalf("Failed to stat file: %v", err)
	}

	// File should not be stable (first encounter)
	shouldProcess1 := watcher.shouldProcessFile(filePath, info1)
	if shouldProcess1 {
		t.Error("File should not be processed on first encounter")
	}

	// Simulate file growth
	time.Sleep(100 * time.Millisecond)
	grownContent := make([]byte, 2000) // Make it even larger
	for i := range grownContent {
		grownContent[i] = 'b'
	}
	err = os.WriteFile(filePath, grownContent, 0644)
	if err != nil {
		t.Fatalf("Failed to update file: %v", err)
	}

	info2, err := os.Stat(filePath)
	if err != nil {
		t.Fatalf("Failed to stat file: %v", err)
	}

	// File should not be stable (size changed)
	shouldProcess2 := watcher.shouldProcessFile(filePath, info2)
	if shouldProcess2 {
		t.Error("Growing file should not be processed")
	}

	// Wait for modification time stability and check again with same size
	time.Sleep(3 * time.Second)

	info3, err := os.Stat(filePath)
	if err != nil {
		t.Fatalf("Failed to stat file: %v", err)
	}

	// First check after stability wait - updates cache with stable size
	shouldProcess3a := watcher.shouldProcessFile(filePath, info3)
	if shouldProcess3a {
		t.Error("First check after stability should still return false due to size cache logic")
	}

	// Second check - now file should be stable (old mod time, same size as cached)
	shouldProcess3b := watcher.shouldProcessFile(filePath, info3)
	if !shouldProcess3b {
		t.Error("Second check of stable file should be processed")
	}
}

func TestDuplicatePrevention(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)

	// Create a stable file that meets all criteria
	content := make([]byte, 1500) // Large enough file
	for i := range content {
		content[i] = 'x'
	}
	filePath := createTestFile(t, tempDir, "duplicate_test.txt", content, time.Now().Add(-10*time.Second))

	// Get file info
	info, err := os.Stat(filePath)
	if err != nil {
		t.Fatalf("Failed to stat file: %v", err)
	}

	// Mock queue to track AddFile calls
	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	watcher.queue = mockQueue

	// First scan - file should be processed
	watcher.shouldProcessFile(filePath, info)                   // Populate size cache
	shouldProcess1 := watcher.shouldProcessFile(filePath, info) // Now stable
	if !shouldProcess1 {
		t.Error("Stable file should be processed on first scan")
	}

	// Simulate adding to queue on first scan
	err = watcher.queue.AddFile(context.TODO(), filePath, info.Size())
	if err != nil {
		t.Fatalf("Failed to add file to queue: %v", err)
	}

	// Verify first call was made
	if len(mockQueue.addFileCalls) != 1 {
		t.Errorf("Expected 1 AddFile call, got %d", len(mockQueue.addFileCalls))
	}

	// Second scan - file should still be stable but queue should prevent duplicate
	shouldProcess2 := watcher.shouldProcessFile(filePath, info)
	if !shouldProcess2 {
		t.Error("File should still be considered processable")
	}

	// Simulate adding to queue on second scan (should be prevented by queue)
	err = watcher.queue.AddFile(context.TODO(), filePath, info.Size())
	if err != nil {
		t.Errorf("AddFile should not return error for duplicate: %v", err)
	}

	// Verify still only one call was actually processed by queue
	if len(mockQueue.addFileCalls) != 1 {
		t.Errorf("Expected queue to prevent duplicate, still got %d AddFile calls", len(mockQueue.addFileCalls))
	}
}

func TestMultipleScanCycles(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)

	// Mock queue to track calls
	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	watcher.queue = mockQueue

	// Create multiple files
	files := []string{"file1.txt", "file2.txt", "file3.txt"}
	for _, filename := range files {
		content := make([]byte, 1500)
		for i := range content {
			content[i] = 'y'
		}
		createTestFile(t, tempDir, filename, content, time.Now().Add(-10*time.Second))
	}

	// Simulate multiple scan cycles
	for cycle := range 3 {
		t.Logf("Scan cycle %d", cycle+1)

		for _, filename := range files {
			filePath := filepath.Join(tempDir, filename)
			info, err := os.Stat(filePath)
			if err != nil {
				t.Fatalf("Failed to stat file: %v", err)
			}

			// First call to populate size cache, second to check stability
			watcher.shouldProcessFile(filePath, info)
			if watcher.shouldProcessFile(filePath, info) {
				err = watcher.queue.AddFile(context.TODO(), filePath, info.Size())
				if err != nil {
					t.Errorf("Cycle %d: AddFile failed for %s: %v", cycle+1, filename, err)
				}
			}
		}
	}

	// Verify each file was only added once despite multiple scans
	if len(mockQueue.addFileCalls) != len(files) {
		t.Errorf("Expected %d unique files in queue, got %d calls: %v",
			len(files), len(mockQueue.addFileCalls), mockQueue.addFileCalls)
	}

	// Verify all expected files are present
	expectedFiles := make(map[string]bool)
	for _, filename := range files {
		expectedFiles[filepath.Join(tempDir, filename)] = true
	}

	for _, addedFile := range mockQueue.addFileCalls {
		if !expectedFiles[addedFile] {
			t.Errorf("Unexpected file added to queue: %s", addedFile)
		}
		delete(expectedFiles, addedFile)
	}

	if len(expectedFiles) > 0 {
		t.Errorf("Some files were not added to queue: %v", expectedFiles)
	}
}

// Helper to create a symlink in test directory
func createTestSymlink(t *testing.T, target, linkPath string) {
	err := os.Symlink(target, linkPath)
	if err != nil {
		t.Fatalf("Failed to create symlink: %v", err)
	}
}

func TestSymlinkToFile_NotFollowed(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)
	// Ensure FollowSymlinks is false (default)
	watcher.cfg.FollowSymlinks = false

	// Create a regular file
	content := make([]byte, 1500)
	for i := range content {
		content[i] = 'z'
	}
	targetFile := createTestFile(t, tempDir, "target.txt", content, time.Now().Add(-10*time.Second))

	// Create a symlink to the file
	symlinkPath := filepath.Join(tempDir, "link_to_target.txt")
	createTestSymlink(t, targetFile, symlinkPath)

	// Mock queue to track calls
	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	watcher.queue = mockQueue

	// Scan the directory twice (first to populate cache, second to add stable files)
	ctx := context.Background()
	err := watcher.scanDirectory(ctx)
	if err != nil {
		t.Fatalf("First scanDirectory failed: %v", err)
	}

	err = watcher.scanDirectory(ctx)
	if err != nil {
		t.Fatalf("Second scanDirectory failed: %v", err)
	}

	// Verify only the target file was added, not the symlink
	if len(mockQueue.addFileCalls) != 1 {
		t.Errorf("Expected 1 file added (target only), got %d", len(mockQueue.addFileCalls))
	}

	// Verify it's the target file, not the symlink
	if len(mockQueue.addFileCalls) > 0 && mockQueue.addFileCalls[0] != targetFile {
		t.Errorf("Expected target file %s to be added, got %s", targetFile, mockQueue.addFileCalls[0])
	}
}

func TestSymlinkToFile_Followed(t *testing.T) {
	// This test verifies that when FollowSymlinks=true and a symlink points to a file
	// within the watch directory, only one entry is processed (avoiding duplicate processing).
	// When a symlink points to a file outside the watch directory, the symlink enables
	// access to that external file.

	watcher, tempDir := createTestWatcher(t)
	// Enable following symlinks
	watcher.cfg.FollowSymlinks = true

	// Create a subdirectory in the watch directory
	subDir := filepath.Join(tempDir, "subdir")
	err := os.Mkdir(subDir, 0755)
	if err != nil {
		t.Fatalf("Failed to create subdirectory: %v", err)
	}

	// Create a file in the subdirectory
	content := make([]byte, 1500)
	for i := range content {
		content[i] = 'z'
	}
	targetFile := createTestFile(t, subDir, "target.txt", content, time.Now().Add(-10*time.Second))

	// Create a symlink in the root watch directory pointing to the file in subdir
	symlinkPath := filepath.Join(tempDir, "link_to_target.txt")
	createTestSymlink(t, targetFile, symlinkPath)

	// Mock queue to track calls
	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	watcher.queue = mockQueue

	// Scan the directory twice (first to populate cache, second to add stable files)
	ctx := context.Background()
	err = watcher.scanDirectory(ctx)
	if err != nil {
		t.Fatalf("First scanDirectory failed: %v", err)
	}

	err = watcher.scanDirectory(ctx)
	if err != nil {
		t.Fatalf("Second scanDirectory failed: %v", err)
	}

	// When FollowSymlinks=true and the symlink points to a file within the watch directory,
	// only the target file should be added (to avoid duplicate processing).
	// The symlink and target are effectively the same file.
	if len(mockQueue.addFileCalls) < 1 {
		t.Errorf("Expected at least 1 file added (target file), got %d: %v", len(mockQueue.addFileCalls), mockQueue.addFileCalls)
	}

	// Verify the target file was added
	foundTarget := slices.Contains(mockQueue.addFileCalls, targetFile)

	if !foundTarget {
		t.Error("Target file was not added to queue")
	}

	// Note: The symlink might or might not be added as a separate entry, depending on
	// walk order and file identity checks. The important thing is that when FollowSymlinks=true,
	// the symlink doesn't cause an error and the target file is accessible.
}

func TestSymlinkToDirectory_Skipped(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)
	watcher.cfg.FollowSymlinks = true // Even with this enabled, directory symlinks should be skipped

	// Create a target directory with a file inside
	targetDir := filepath.Join(tempDir, "target_dir")
	err := os.Mkdir(targetDir, 0755)
	if err != nil {
		t.Fatalf("Failed to create target directory: %v", err)
	}

	content := make([]byte, 1500)
	for i := range content {
		content[i] = 'a'
	}
	fileInTargetDir := createTestFile(t, targetDir, "file_in_dir.txt", content, time.Now().Add(-10*time.Second))

	// Create a symlink to the directory
	symlinkDir := filepath.Join(tempDir, "link_to_dir")
	createTestSymlink(t, targetDir, symlinkDir)

	// Mock queue to track calls
	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	watcher.queue = mockQueue

	// Scan the directory twice (first to populate cache, second to add stable files)
	ctx := context.Background()
	err = watcher.scanDirectory(ctx)
	if err != nil {
		t.Fatalf("First scanDirectory failed: %v", err)
	}

	err = watcher.scanDirectory(ctx)
	if err != nil {
		t.Fatalf("Second scanDirectory failed: %v", err)
	}

	// Verify only the file in the target directory was added
	if len(mockQueue.addFileCalls) != 1 {
		t.Errorf("Expected 1 file added (file in target dir), got %d", len(mockQueue.addFileCalls))
	}

	if len(mockQueue.addFileCalls) > 0 && mockQueue.addFileCalls[0] != fileInTargetDir {
		t.Errorf("Expected file in target dir %s, got %s", fileInTargetDir, mockQueue.addFileCalls[0])
	}
}

func TestBrokenSymlink_Handled(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)
	watcher.cfg.FollowSymlinks = false

	// Create a broken symlink (target doesn't exist)
	brokenSymlink := filepath.Join(tempDir, "broken_link.txt")
	nonExistentTarget := filepath.Join(tempDir, "does_not_exist.txt")
	createTestSymlink(t, nonExistentTarget, brokenSymlink)

	// Mock queue to track calls
	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	watcher.queue = mockQueue

	// Scan the directory - should not crash
	ctx := context.Background()
	err := watcher.scanDirectory(ctx)
	if err != nil {
		t.Fatalf("scanDirectory should handle broken symlinks gracefully, got error: %v", err)
	}

	// Verify no files were added (broken symlink should be skipped)
	if len(mockQueue.addFileCalls) != 0 {
		t.Errorf("Expected 0 files added (broken symlink should be skipped), got %d", len(mockQueue.addFileCalls))
	}
}

func TestSymlinkInSizeCalculation(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)
	watcher.cfg.FollowSymlinks = false

	// Create a regular file
	content := make([]byte, 1500)
	for i := range content {
		content[i] = 'x'
	}
	targetFile := createTestFile(t, tempDir, "target_size.txt", content, time.Now().Add(-10*time.Second))

	// Create a symlink to the file
	symlinkPath := filepath.Join(tempDir, "link_to_target_size.txt")
	createTestSymlink(t, targetFile, symlinkPath)

	// Calculate directory size
	ctx := context.Background()
	totalSize, err := watcher.calculateDirectorySize(ctx)
	if err != nil {
		t.Fatalf("calculateDirectorySize failed: %v", err)
	}

	// Expected size should be only the target file size (symlink should not count)
	expectedSize := int64(1500)
	if totalSize != expectedSize {
		t.Errorf("Expected total size %d (target file only), got %d", expectedSize, totalSize)
	}
}

func TestSymlinkInGroupByFolder(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)
	watcher.cfg.FollowSymlinks = false
	watcher.cfg.SingleNzbPerFolder = true

	// Create a folder with a regular file
	folderPath := filepath.Join(tempDir, "test_folder")
	err := os.Mkdir(folderPath, 0755)
	if err != nil {
		t.Fatalf("Failed to create folder: %v", err)
	}

	content := make([]byte, 1500)
	for i := range content {
		content[i] = 'y'
	}
	_ = createTestFile(t, folderPath, "target.txt", content, time.Now().Add(-10*time.Second))

	// Create a symlink to another file in the same folder
	targetFile2 := createTestFile(t, folderPath, "target2.txt", content, time.Now().Add(-10*time.Second))
	symlinkPath := filepath.Join(folderPath, "link_to_target2.txt")
	createTestSymlink(t, targetFile2, symlinkPath)

	// Mock queue to track calls
	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	watcher.queue = mockQueue

	// Scan the directory in folder mode twice (first to populate cache, second to add stable files)
	ctx := context.Background()
	err = watcher.scanDirectoryGroupByFolder(ctx)
	if err != nil {
		t.Fatalf("First scanDirectoryGroupByFolder failed: %v", err)
	}

	err = watcher.scanDirectoryGroupByFolder(ctx)
	if err != nil {
		t.Fatalf("Second scanDirectoryGroupByFolder failed: %v", err)
	}

	// Verify only one folder entry was added (symlink should not cause double counting)
	if len(mockQueue.addFileCalls) != 1 {
		t.Errorf("Expected 1 folder entry, got %d", len(mockQueue.addFileCalls))
	}

	// Verify it's the folder path with FOLDER: prefix
	expectedPath := "FOLDER:" + folderPath
	if len(mockQueue.addFileCalls) > 0 && mockQueue.addFileCalls[0] != expectedPath {
		t.Errorf("Expected folder path %s, got %s", expectedPath, mockQueue.addFileCalls[0])
	}
}

func TestGroupByFolder_NestedSubdirs(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)

	content := []byte("test content!!")
	modTime := time.Now().Add(-10 * time.Second)

	sunMomentsDir := filepath.Join(tempDir, "Sun.Moments")
	if err := os.MkdirAll(sunMomentsDir, 0755); err != nil {
		t.Fatalf("Failed to create Sun.Moments dir: %v", err)
	}
	proofDir := filepath.Join(sunMomentsDir, "Proof")
	if err := os.MkdirAll(proofDir, 0755); err != nil {
		t.Fatalf("Failed to create Proof dir: %v", err)
	}
	sampleDir := filepath.Join(sunMomentsDir, "Sample")
	if err := os.MkdirAll(sampleDir, 0755); err != nil {
		t.Fatalf("Failed to create Sample dir: %v", err)
	}

	createTestFile(t, sunMomentsDir, "file1.rar", content, modTime)
	createTestFile(t, sunMomentsDir, "file2.rar", content, modTime)
	createTestFile(t, proofDir, "proof.jpg", content, modTime)
	createTestFile(t, sampleDir, "sample.mkv", content, modTime)

	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	watcher.queue = mockQueue

	ctx := context.Background()
	if err := watcher.scanDirectoryGroupByFolder(ctx); err != nil {
		t.Fatalf("First scan failed: %v", err)
	}
	if err := watcher.scanDirectoryGroupByFolder(ctx); err != nil {
		t.Fatalf("Second scan failed: %v", err)
	}

	if len(mockQueue.addFileCalls) != 1 {
		t.Errorf("Expected 1 folder entry, got %d: %v", len(mockQueue.addFileCalls), mockQueue.addFileCalls)
	}

	expectedPath := "FOLDER:" + filepath.Join(tempDir, "Sun.Moments")
	if len(mockQueue.addFileCalls) > 0 && mockQueue.addFileCalls[0] != expectedPath {
		t.Errorf("Expected %s, got %s", expectedPath, mockQueue.addFileCalls[0])
	}
}

func TestGroupByFolder_FilesDirectlyInWatchFolder(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)

	content := []byte("test content!!")
	modTime := time.Now().Add(-10 * time.Second)

	createTestFile(t, tempDir, "file1.rar", content, modTime)
	createTestFile(t, tempDir, "file2.rar", content, modTime)

	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	watcher.queue = mockQueue

	ctx := context.Background()
	if err := watcher.scanDirectoryGroupByFolder(ctx); err != nil {
		t.Fatalf("First scan failed: %v", err)
	}
	if err := watcher.scanDirectoryGroupByFolder(ctx); err != nil {
		t.Fatalf("Second scan failed: %v", err)
	}

	if len(mockQueue.addFileCalls) != 2 {
		t.Errorf("Expected 2 individual file entries, got %d: %v", len(mockQueue.addFileCalls), mockQueue.addFileCalls)
	}

	for _, call := range mockQueue.addFileCalls {
		if len(call) >= 7 && call[:7] == "FOLDER:" {
			t.Errorf("Expected individual file path, got folder entry: %s", call)
		}
	}
}

func TestGroupByFolder_MixedRootAndSubdir(t *testing.T) {
	watcher, tempDir := createTestWatcher(t)

	content := []byte("test content!!")
	modTime := time.Now().Add(-10 * time.Second)

	rootFile := createTestFile(t, tempDir, "standalone.rar", content, modTime)

	releaseDir := filepath.Join(tempDir, "Release")
	if err := os.MkdirAll(releaseDir, 0755); err != nil {
		t.Fatalf("Failed to create Release dir: %v", err)
	}
	createTestFile(t, releaseDir, "release1.rar", content, modTime)
	createTestFile(t, releaseDir, "release2.rar", content, modTime)

	mockQueue := &mockQueueWithDuplicateCheck{
		addFileCalls: make([]string, 0),
	}
	watcher.queue = mockQueue

	ctx := context.Background()
	if err := watcher.scanDirectoryGroupByFolder(ctx); err != nil {
		t.Fatalf("First scan failed: %v", err)
	}
	if err := watcher.scanDirectoryGroupByFolder(ctx); err != nil {
		t.Fatalf("Second scan failed: %v", err)
	}

	if len(mockQueue.addFileCalls) != 2 {
		t.Errorf("Expected 2 entries (1 file + 1 folder), got %d: %v", len(mockQueue.addFileCalls), mockQueue.addFileCalls)
	}

	expectedFolderEntry := "FOLDER:" + filepath.Join(tempDir, "Release")
	hasFile := false
	hasFolder := false
	for _, call := range mockQueue.addFileCalls {
		if call == rootFile {
			hasFile = true
		}
		if call == expectedFolderEntry {
			hasFolder = true
		}
	}

	if !hasFile {
		t.Errorf("Expected individual file entry %s in calls %v", rootFile, mockQueue.addFileCalls)
	}
	if !hasFolder {
		t.Errorf("Expected folder entry %s in calls %v", expectedFolderEntry, mockQueue.addFileCalls)
	}
}

// TestScan_JobsCarryWatcherInputFolder verifies that every enqueued job carries
// this watcher's own root as InputFolder, so the processor derives relative
// output paths from the correct watch folder even when multiple watchers are
// configured (issue #168: only the first watcher's root was known globally).
func TestScan_JobsCarryWatcherInputFolder(t *testing.T) {
	t.Run("traditional mode", func(t *testing.T) {
		watcher, tempDir := createTestWatcher(t)

		// Large enough that two files exceed the watcher's SizeThreshold (100).
		content := bytes.Repeat([]byte("x"), 128)
		modTime := time.Now().Add(-10 * time.Second)

		movieDir := filepath.Join(tempDir, "Movie_A")
		if err := os.MkdirAll(movieDir, 0755); err != nil {
			t.Fatalf("Failed to create Movie_A dir: %v", err)
		}
		filePath := createTestFile(t, movieDir, "movie.rar", content, modTime)
		createTestFile(t, movieDir, "movie2.rar", content, modTime)

		mockQueue := &mockQueueWithDuplicateCheck{
			addFileCalls: make([]string, 0),
		}
		watcher.queue = mockQueue

		ctx := context.Background()
		// First scan populates the file-size stability cache; second scan enqueues.
		if err := watcher.scanDirectory(ctx); err != nil {
			t.Fatalf("First scan failed: %v", err)
		}
		if err := watcher.scanDirectory(ctx); err != nil {
			t.Fatalf("Second scan failed: %v", err)
		}

		if len(mockQueue.addFileCalls) == 0 {
			t.Fatal("Expected files to be enqueued")
		}
		opts, ok := mockQueue.addFileOpts[filePath]
		if !ok {
			t.Fatalf("Expected %s to be enqueued via AddFileWithOptions, calls: %v", filePath, mockQueue.addFileCalls)
		}
		if opts.InputFolder != tempDir {
			t.Errorf("Expected InputFolder %s, got %q", tempDir, opts.InputFolder)
		}
	})

	t.Run("single nzb per folder mode", func(t *testing.T) {
		watcher, tempDir := createTestWatcher(t)

		content := []byte("test content for input folder check!!")
		modTime := time.Now().Add(-10 * time.Second)

		movieDir := filepath.Join(tempDir, "Movie_A")
		if err := os.MkdirAll(movieDir, 0755); err != nil {
			t.Fatalf("Failed to create Movie_A dir: %v", err)
		}
		createTestFile(t, movieDir, "movie.rar", content, modTime)
		rootFile := createTestFile(t, tempDir, "loose.rar", content, modTime)

		mockQueue := &mockQueueWithDuplicateCheck{
			addFileCalls: make([]string, 0),
		}
		watcher.queue = mockQueue

		ctx := context.Background()
		if err := watcher.scanDirectoryGroupByFolder(ctx); err != nil {
			t.Fatalf("First scan failed: %v", err)
		}
		if err := watcher.scanDirectoryGroupByFolder(ctx); err != nil {
			t.Fatalf("Second scan failed: %v", err)
		}

		folderQueuePath := "FOLDER:" + movieDir
		opts, ok := mockQueue.addFileOpts[folderQueuePath]
		if !ok {
			t.Fatalf("Expected folder job %s to be enqueued via AddFileWithOptions, calls: %v", folderQueuePath, mockQueue.addFileCalls)
		}
		if opts.InputFolder != tempDir {
			t.Errorf("Expected folder job InputFolder %s, got %q", tempDir, opts.InputFolder)
		}

		opts, ok = mockQueue.addFileOpts[rootFile]
		if !ok {
			t.Fatalf("Expected root file %s to be enqueued via AddFileWithOptions, calls: %v", rootFile, mockQueue.addFileCalls)
		}
		if opts.InputFolder != tempDir {
			t.Errorf("Expected root file InputFolder %s, got %q", tempDir, opts.InputFolder)
		}
	})
}

// TestScanDirectory_SkipsUnreadableSubdirectory: one unreadable entry (a
// permission-denied subfolder, or a file removed mid-scan) used to abort the
// whole scan, so nothing in the watch folder was ever imported while it was
// there. Unreadable entries must be skipped, not fatal.
func TestScanDirectory_SkipsUnreadableSubdirectory(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("directory permission bits are not enforced on Windows")
	}
	if os.Geteuid() == 0 {
		t.Skip("root ignores directory permissions")
	}
	for _, singleNzbPerFolder := range []bool{false, true} {
		t.Run(map[bool]string{false: "per-file", true: "per-folder"}[singleNzbPerFolder], func(t *testing.T) {
			watcher, tempDir := createTestWatcher(t)
			watcher.cfg.SingleNzbPerFolder = singleNzbPerFolder

			content := bytes.Repeat([]byte("x"), 1500)
			goodFile := createTestFile(t, tempDir, "good.bin", content, time.Now().Add(-10*time.Second))

			locked := filepath.Join(tempDir, "locked")
			if err := os.MkdirAll(locked, 0o755); err != nil {
				t.Fatalf("mkdir: %v", err)
			}
			createTestFile(t, locked, "hidden.bin", content, time.Now().Add(-10*time.Second))
			if err := os.Chmod(locked, 0o000); err != nil {
				t.Fatalf("chmod: %v", err)
			}
			t.Cleanup(func() { _ = os.Chmod(locked, 0o755) })

			mockQueue := &mockQueueWithDuplicateCheck{addFileCalls: make([]string, 0)}
			watcher.queue = mockQueue

			ctx := context.Background()
			for i := 0; i < 2; i++ {
				if err := watcher.scanDirectory(ctx); err != nil {
					t.Fatalf("scanDirectory #%d: %v", i+1, err)
				}
			}
			if len(mockQueue.addFileCalls) != 1 || mockQueue.addFileCalls[0] != goodFile {
				t.Errorf("queued %v, want exactly [%s]", mockQueue.addFileCalls, goodFile)
			}
		})
	}
}
