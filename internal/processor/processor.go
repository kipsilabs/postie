package processor

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"time"

	"encoding/json"

	"github.com/javi11/postie/internal/config"
	"github.com/javi11/postie/internal/pausable"
	"github.com/javi11/postie/internal/pool"
	"github.com/javi11/postie/internal/poster"
	"github.com/javi11/postie/internal/progress"
	"github.com/javi11/postie/internal/queue"
	"github.com/javi11/postie/internal/transferstore"
	"github.com/javi11/postie/pkg/fileinfo"
	"github.com/javi11/postie/pkg/postie"
	"maragu.dev/goqite"
)

const maxRetries = 3

type Processor struct {
	queue        *queue.Queue
	config       config.Config
	cfg          config.QueueConfig
	poolManager  *pool.Manager
	outputFolder string
	isRunning    bool
	runningMux   sync.Mutex
	// Track running jobs and their contexts for cancellation
	runningJobs               map[string]*RunningJob
	jobsMux                   sync.RWMutex
	jobsWg                    sync.WaitGroup // WaitGroup to track running jobs
	deleteOriginalFile        bool
	deleteDelay               time.Duration
	maintainOriginalExtension bool
	watchFolder               string // Path to the watch folder for maintaining folder structure
	followSymlinks            bool   // Control whether to follow symlinks when collecting folder files
	// Reserved paths - tracks jobs claimed from queue but not yet in runningJobs
	// This prevents race conditions where watcher could re-add files during the gap
	reservedPaths map[string]time.Time
	reservedMux   sync.RWMutex
	// Pause/resume functionality
	isPaused  bool
	pausedMux sync.RWMutex
	// Auto-pause functionality
	isAutoPaused          bool
	autoPauseReason       string
	isAutoBlockingNewJobs bool // blocks new jobs without pausing running ones
	autoPausedMux         sync.RWMutex
	providerCheckTicker   *time.Ticker
	// providerErrorBaseline holds each provider's cumulative error count as of
	// the previous availability check. Only touched by the monitor goroutine.
	providerErrorBaseline map[string]int64
	providerCheckCtx      context.Context
	providerCheckCancel   context.CancelFunc
	// Callback to check if processor can start new items
	canProcessNextItem func() bool
	// Callback when job fails permanently
	onJobError func(fileName, errorMessage string)
	// Callback when any job finishes (success or deferred)
	onJobComplete func()
	// Shutdown flag to prevent new operations during close
	isShuttingDown bool
	// transferRuntime owns process-wide upload resources (currently the PAR2
	// scheduler) shared by every job, so limits hold regardless of queue
	// concurrency. May be nil, in which case each Postie uses private resources.
	transferRuntime *postie.Runtime
	// startVerificationOnce guards starting the durable verification service.
	startVerificationOnce sync.Once
}

type ProcessorOptions struct {
	Queue                     *queue.Queue
	Config                    config.Config
	QueueConfig               config.QueueConfig
	PoolManager               *pool.Manager
	OutputFolder              string
	DeleteOriginalFile        bool
	DeleteDelay               time.Duration // delay before deleting original files
	MaintainOriginalExtension bool
	WatchFolder               string
	FollowSymlinks            bool                                // Control whether to follow symlinks when collecting folder files
	CanProcessNextItem        func() bool                         // Callback to check if processor can start new items
	OnJobError                func(fileName, errorMessage string) // Callback when job fails permanently
	OnJobComplete             func()                              // Callback when any job finishes (success or deferred)
}
type RunningJobDetails struct {
	ID       string                   `json:"id"`
	Path     string                   `json:"path"`
	FileName string                   `json:"fileName"`
	Size     int64                    `json:"size"`
	Progress []progress.ProgressState `json:"progress"`
}

type RunningJob struct {
	RunningJobDetails
	Progress    progress.JobProgress
	cancel      context.CancelFunc
	pausableCtx *pausable.Context
}

// RunningJobItem represents a running job for the frontend (kept for backward compatibility)
type RunningJobItem struct {
	ID string `json:"id"`
}

func New(opts ProcessorOptions) *Processor {
	// Create context for provider monitoring
	providerCtx, providerCancel := context.WithCancel(context.Background())

	processor := &Processor{
		queue:                     opts.Queue,
		config:                    opts.Config,
		cfg:                       opts.QueueConfig,
		poolManager:               opts.PoolManager,
		outputFolder:              opts.OutputFolder,
		runningJobs:               make(map[string]*RunningJob),
		reservedPaths:             make(map[string]time.Time),
		deleteOriginalFile:        opts.DeleteOriginalFile,
		deleteDelay:               opts.DeleteDelay,
		maintainOriginalExtension: opts.MaintainOriginalExtension,
		watchFolder:               opts.WatchFolder,
		followSymlinks:            opts.FollowSymlinks,
		providerCheckCtx:          providerCtx,
		providerCheckCancel:       providerCancel,
		canProcessNextItem:        opts.CanProcessNextItem,
		onJobError:                opts.OnJobError,
		onJobComplete:             opts.OnJobComplete,
	}

	// Create the process-wide transfer runtime so resource limits (PAR2
	// concurrency today, upload engine and verification later) hold regardless
	// of queue concurrency. On failure, fall back to per-job resources so the
	// processor still functions.
	if opts.Config != nil {
		// Durable transfer store + manifest dir enable manifest recording and
		// crash-safe verification. Manifests live alongside the database file.
		var transferStore *transferstore.Store
		var manifestDir string
		if opts.Queue != nil {
			if db := opts.Queue.DB(); db != nil {
				transferStore = transferstore.New(db)
				manifestDir = filepath.Join(filepath.Dir(opts.Config.GetDatabaseConfig().DatabasePath), "transfer-manifests")
				// One-time migration of pre-durable deferred checks into the
				// durable verification_failures table (STAT-only).
				if migrated, err := transferStore.MigrateLegacyPendingChecks(providerCtx); err != nil {
					slog.Warn("Failed to migrate legacy pending article checks", "error", err)
				} else if migrated > 0 {
					slog.Info("Migrated legacy pending article checks to durable verification", "count", migrated)
				}
			}
		}
		if rt, err := postie.NewRuntime(providerCtx, opts.Config, opts.PoolManager, transferStore, manifestDir, opts.Queue); err != nil {
			slog.Error("Failed to create transfer runtime; falling back to per-job PAR2 scheduling", "error", err)
		} else {
			processor.transferRuntime = rt
		}
	}

	// Start provider availability monitoring if we have a pool manager
	if opts.PoolManager != nil {
		go processor.monitorProviderAvailability()
	}

	return processor
}

// Start begins processing files from the queue
func (p *Processor) Start(ctx context.Context) error {
	// Start the durable verification service once. It runs independently of the
	// queue processing loop (and of pause), verifying completed transfers and
	// re-posting missing articles in the background. No-op when no runtime/store.
	p.startVerificationOnce.Do(func() {
		if p.transferRuntime != nil {
			go p.transferRuntime.RunVerification(ctx)
		}
	})

	processTicker := time.NewTicker(time.Second * 2) // Process queue frequently
	defer processTicker.Stop()

	// Diagnostic watchdog: periodically log goroutine count and heap usage at
	// debug level so silent OOM-kills (e.g. on memory-constrained NAS hosts)
	// leave a trend line in the logs before the process dies. See issue #184.
	watchdogTicker := time.NewTicker(10 * time.Second)
	defer watchdogTicker.Stop()

	// Main processing loop
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-processTicker.C:
			if err := p.processQueueItems(ctx); err != nil {
				slog.ErrorContext(ctx, "Error processing queue", "error", err)
			}
		case <-watchdogTicker.C:
			var ms runtime.MemStats
			runtime.ReadMemStats(&ms)
			slog.DebugContext(ctx, "Runtime watchdog",
				"goroutines", runtime.NumGoroutine(),
				"heap_alloc_bytes", ms.HeapAlloc,
				"heap_inuse_bytes", ms.HeapInuse,
				"sys_bytes", ms.Sys,
			)
		}
	}
}

func (p *Processor) processQueueItems(ctx context.Context) error {
	// Check if processor is paused
	p.pausedMux.RLock()
	paused := p.isPaused
	p.pausedMux.RUnlock()

	if paused {
		return nil // Skip processing when paused
	}

	// Check if new jobs are auto-blocked due to provider unavailability.
	// Unlike manual pause, this does NOT affect already-running jobs.
	p.autoPausedMux.RLock()
	autoBlocked := p.isAutoBlockingNewJobs
	p.autoPausedMux.RUnlock()

	if autoBlocked {
		slog.DebugContext(ctx, "Processor waiting - providers unavailable, blocking new jobs")
		return nil
	}

	// Check if we can process next item (e.g., pending config changes)
	if p.canProcessNextItem != nil && !p.canProcessNextItem() {
		slog.DebugContext(ctx, "Processor waiting - cannot process new items (callback returned false)")
		return nil // Skip processing when callback indicates we should wait
	}

	if p.isRunning {
		return nil
	}

	p.isRunning = true
	defer func() {
		p.isRunning = false
	}()

	// Process items with configurable concurrency
	semaphore := make(chan struct{}, p.cfg.MaxConcurrentUploads)
	var wg sync.WaitGroup

	// Process multiple items concurrently
	for range p.cfg.MaxConcurrentUploads {
		wg.Add(1)
		semaphore <- struct{}{} // Acquire

		go func() {
			defer wg.Done()
			defer func() { <-semaphore }() // Release

			if err := p.processNextItem(ctx); err != nil {
				if !errors.Is(err, context.Canceled) {
					slog.ErrorContext(ctx, "Error processing item", "error", err)
				}
			}
		}()
	}

	wg.Wait()
	return nil
}

func (p *Processor) processNextItem(ctx context.Context) error {
	// Check if processor is shutting down
	select {
	case <-ctx.Done():
		return ctx.Err()
	default:
	}

	// Check if processor is shutting down or pool manager is unavailable
	p.runningMux.Lock()
	shuttingDown := p.isShuttingDown
	poolManager := p.poolManager
	p.runningMux.Unlock()

	if shuttingDown {
		return nil
	}

	if poolManager == nil {
		slog.WarnContext(ctx, "Pool manager is not available, waiting for server configuration")
		return nil
	}

	// Aggregate-size threshold: hold off picking up new jobs until the total
	// pending bytes meet the configured floor. 0 disables gating (default).
	if minSize := int64(p.cfg.MinSizeToStart); minSize > 0 {
		pending, err := p.queue.PendingTotalSize(ctx)
		if err != nil {
			slog.WarnContext(ctx, "Failed to read pending queue size, processing anyway", "error", err)
		} else if pending < minSize {
			slog.DebugContext(ctx, "Queue below min_size_to_start, holding",
				"pending", pending, "threshold", minSize)
			return nil
		}
	}

	// Get next item from queue
	msg, job, err := p.queue.ReceiveFile(ctx)
	if err != nil {
		return err
	}

	// If no message available, return without error
	if msg == nil {
		return nil
	}

	// IMMEDIATELY reserve the path to prevent duplicates from watcher
	// This closes the race condition gap between ReceiveFile() and processFile()
	p.reservePath(job.Path)

	slog.Info("Processing file", "msg", msg.ID, "path", job.Path, "priority", job.Priority)

	// Process the file and get both NZB path and postie instance
	actualNzbPath, jobPostie, err := p.processFile(ctx, msg, job)

	// Check for DeferredCheckError first - this is a non-fatal error
	var deferredErr *poster.DeferredCheckError
	if err != nil && errors.As(err, &deferredErr) {
		// Non-fatal: articles were posted and NZB was generated, but some articles
		// couldn't be verified immediately. Store them for deferred checking.
		slog.InfoContext(ctx, "Some articles deferred for later verification",
			"path", job.Path,
			"deferred_articles", len(deferredErr.FailedArticles),
			"total_articles", deferredErr.TotalArticles,
			"nzb_path", actualNzbPath)

		// Mark file as completed (NZB was generated successfully)
		if completeErr := p.queue.CompleteFile(ctx, msg.ID, actualNzbPath, job); completeErr != nil {
			slog.ErrorContext(ctx, "Error marking file as completed", "error", completeErr, "path", job.Path)
			return completeErr
		}

		// Store deferred articles in the database for the background worker
		completedItemID := string(msg.ID)
		deferredDelay := p.config.GetPostCheckConfig().DeferredCheckDelay.ToDuration()
		nextRetryAt := time.Now().Add(deferredDelay)

		var pendingChecks []queue.PendingArticleCheck
		for _, art := range deferredErr.FailedArticles {
			groupsJSON, _ := json.Marshal(art.Groups)
			pendingChecks = append(pendingChecks, queue.PendingArticleCheck{
				MessageID:   art.MessageID,
				Groups:      string(groupsJSON),
				NextRetryAt: nextRetryAt,
			})
		}

		if addErr := p.queue.AddPendingArticleChecks(ctx, completedItemID, pendingChecks); addErr != nil {
			slog.ErrorContext(ctx, "Failed to store deferred article checks", "error", addErr)
		} else {
			// Update the completed item verification status
			if statusErr := p.queue.UpdateCompletedItemVerificationStatus(ctx, completedItemID, "pending_verification"); statusErr != nil {
				slog.ErrorContext(ctx, "Failed to update verification status", "error", statusErr)
			}
		}

		// Execute post upload script if configured (NZB is valid)
		sourcePath := strings.TrimPrefix(job.Path, "FOLDER:")
		if scriptErr := jobPostie.ExecutePostUploadScript(ctx, actualNzbPath, sourcePath, completedItemID); scriptErr != nil {
			slog.ErrorContext(ctx, "Post upload script execution failed", "error", scriptErr, "nzbPath", actualNzbPath)
		}

		if p.onJobComplete != nil {
			p.onJobComplete()
		}

		return nil
	}

	if err != nil {
		// Unreserve the path since processing failed before reaching runningJobs
		p.unreservePath(job.Path)

		if errors.Is(err, context.Canceled) {
			slog.Info("Job cancelled", "msg", msg.ID, "path", job.Path)

			// Only clean up if this is a user cancellation (not app shutdown).
			// If the outer ctx is also done, keep in_progress_items for crash recovery on next start.
			if ctx.Err() == nil {
				if cancelErr := p.queue.CancelFile(ctx, msg.ID); cancelErr != nil {
					slog.ErrorContext(ctx, "Failed to clean up in-progress item after cancel",
						"error", cancelErr, "id", string(msg.ID))
				}
				// The job will not be resumed, so its durable recovery data
				// (planned rows, manifests) would otherwise be orphaned.
				if discardErr := p.transferRuntime.DiscardTransfer(ctx, job.TransferID); discardErr != nil {
					slog.WarnContext(ctx, "Failed to discard transfer after cancel",
						"error", discardErr, "transfer", job.TransferID)
				}
			}

			return nil
		}
		// Handle error with retry logic - re-add job to queue
		return p.handleProcessingError(ctx, msg, job, string(msg.ID), err)
	}

	// Use the actual NZB path returned by the postie.Post method
	// Mark as completed with the NZB path and job data
	if err := p.queue.CompleteFile(ctx, msg.ID, actualNzbPath, job); err != nil {
		slog.ErrorContext(ctx, "Error marking file as completed", "error", err, "path", job.Path)
		return err
	}

	// In durable mode the upload is complete but verification runs in the
	// background, so the item is pending verification rather than verified.
	// Link the transfer's files to this completed item so the verification
	// service can reflect the final verified/failed status back onto it.
	if p.durableMode() {
		// Link BEFORE flipping the status to pending_verification: a crash
		// between the two would otherwise strand an item the verification
		// service can never find (it discovers work through transfer_files).
		if linkErr := p.transferRuntime.TransferStore().SetCompletedItemForTransfer(ctx, job.TransferID, string(msg.ID)); linkErr != nil {
			slog.ErrorContext(ctx, "Failed to link completed item to transfer", "error", linkErr, "transfer", job.TransferID)
		} else if statusErr := p.queue.UpdateCompletedItemVerificationStatus(ctx, string(msg.ID), "pending_verification"); statusErr != nil {
			slog.ErrorContext(ctx, "Failed to set pending_verification status", "error", statusErr, "path", job.Path)
		}
	}

	// Execute post upload script if configured. In durable mode the script is
	// deferred until verification succeeds (run by the transfer cleaner), so it
	// only fires here in standalone mode.
	// Note: We don't return the error here to avoid failing the completion if the script fails;
	// the failure is tracked in the database for retry.
	if !p.durableMode() {
		sourcePath := strings.TrimPrefix(job.Path, "FOLDER:")
		if scriptErr := jobPostie.ExecutePostUploadScript(ctx, actualNzbPath, sourcePath, string(msg.ID)); scriptErr != nil {
			slog.ErrorContext(ctx, "Post upload script execution failed", "error", scriptErr, "nzbPath", actualNzbPath)
		}
	}

	if p.onJobComplete != nil {
		p.onJobComplete()
	}

	return nil
}

func (p *Processor) processFile(ctx context.Context, msg *goqite.Message, job *queue.FileJob) (string, *postie.Postie, error) {
	// Check if this is a folder job
	isFolder := strings.HasPrefix(job.Path, "FOLDER:")
	var fileName string
	var filesToProcess []fileinfo.FileInfo
	var folderPath string

	if isFolder {
		// Extract the actual folder path
		folderPath = strings.TrimPrefix(job.Path, "FOLDER:")
		fileName = filepath.Base(folderPath)

		// Collect all files in the folder
		files, err := p.collectFilesInFolder(folderPath)
		if err != nil {
			return "", nil, fmt.Errorf("failed to collect files in folder %s: %w", folderPath, err)
		}
		if len(files) == 0 {
			return "", nil, fmt.Errorf("no files found in folder %s", folderPath)
		}
		filesToProcess = files

		slog.InfoContext(ctx, "Processing folder as single NZB", "folder", fileName, "files", len(files))
	} else {
		// Single file processing
		fileName = getFileName(job.Path)
		filesToProcess = []fileinfo.FileInfo{
			{
				Path: job.Path,
				Size: uint64(job.Size),
			},
		}
	}

	jobID := string(msg.ID)

	// Create a context for this specific job that can be cancelled independently
	jobCtx, jobCancel := context.WithCancel(ctx)

	// Create a pausable context wrapper
	pausableCtx := pausable.NewContext(jobCtx)

	// Track this job for potential cancellation and pausing
	p.jobsMux.Lock()
	// Track detailed job information
	progressJob := progress.NewProgressJob(jobID)
	defer progressJob.Close()

	// Add to WaitGroup before adding to running jobs
	p.jobsWg.Add(1)

	p.runningJobs[jobID] = &RunningJob{
		RunningJobDetails: RunningJobDetails{
			ID:       jobID,
			Path:     job.Path,
			FileName: fileName,
			Size:     job.Size,
		},
		Progress:    progressJob,
		cancel:      jobCancel,
		pausableCtx: pausableCtx,
	}

	// Transfer from reserved to running state - the path is now tracked in runningJobs
	// so we can remove it from reservedPaths
	p.unreservePath(job.Path)

	// Apply current pause state to new job
	if p.isPaused {
		pausableCtx.Pause()
		// Also set progress as paused for new jobs
		progressJob.SetAllPaused(true)
	}
	p.jobsMux.Unlock()

	// Cleanup function to remove from tracking
	defer func() {
		p.jobsMux.Lock()
		delete(p.runningJobs, jobID)
		p.jobsMux.Unlock()
		jobCancel()     // Ensure context is cancelled
		p.jobsWg.Done() // Signal job completion to WaitGroup
	}()

	// Double-check that pool manager is still available before creating postie
	p.runningMux.Lock()
	poolManager := p.poolManager
	p.runningMux.Unlock()

	if poolManager == nil {
		return "", nil, fmt.Errorf("pool manager is not available for job %s", jobID)
	}

	// Create a postie instance for this job, sharing the process-wide transfer
	// runtime so PAR2 (and later upload/verification) limits are enforced
	// globally rather than per queue job.
	jobPostie, err := postie.NewWithRuntime(jobCtx, p.config, poolManager, progressJob, p.queue, p.transferRuntime, job.TransferID)
	if err != nil {
		return "", nil, fmt.Errorf("failed to create postie instance for job %s: %w", jobID, err)
	}
	defer jobPostie.Close()

	// Resolve the delete-original policy (job override wins over the global
	// setting) and hand it to Postie so it is persisted into the transfer's
	// cleanup policy at completion. In durable mode the actual deletion happens
	// only after verification succeeds.
	deleteOriginal := p.deleteOriginalFile
	if job.DeleteOriginal != nil {
		deleteOriginal = *job.DeleteOriginal
	}
	jobPostie.SetDeleteOriginal(deleteOriginal)

	// Determine the input folder for maintaining folder structure
	var inputFolder string
	switch {
	case job.InputFolder != "":
		// Job-supplied root (e.g., HTTP API "relative_path"): use it verbatim
		// so the NZB output mirrors the directory tree below it.
		inputFolder = job.InputFolder
		slog.DebugContext(jobCtx, "Using job-supplied input folder",
			"inputFolder", inputFolder, "filePath", job.Path)
	case isFolder:
		// For folder processing, use the parent directory of the folder
		inputFolder = filepath.Dir(folderPath)
	case p.watchFolder != "" && isWithinPath(job.Path, p.watchFolder):
		// For files from the watcher, use the watch folder as root to maintain structure
		inputFolder = p.watchFolder
		slog.DebugContext(jobCtx, "Using watch folder as root for folder structure",
			"watchFolder", p.watchFolder, "filePath", job.Path)
	default:
		// For manually added files, use the directory containing the file
		inputFolder = filepath.Dir(job.Path)
		slog.DebugContext(jobCtx, "Using file directory as root",
			"inputFolder", inputFolder, "filePath", job.Path)
	}

	// Post the files using the job-specific postie instance with pausable context
	// Pass isFolder flag to force folder mode (single NZB) for explicit folder uploads
	actualNzbPath, err := jobPostie.Post(pausableCtx, filesToProcess, inputFolder, p.outputFolder, isFolder)
	if err != nil {
		// DeferredCheckError is non-fatal: the NZB was generated and jobPostie is valid.
		// Return them so the caller can execute the post-upload script and store deferred checks.
		var deferredErr *poster.DeferredCheckError
		if errors.As(err, &deferredErr) {
			return actualNzbPath, jobPostie, err
		}
		return "", nil, err
	}

	// Delete the original files if configured (deleteOriginal resolved above).
	// In durable mode, the upload is complete but NOT yet verified. Deleting the
	// original here would destroy the only recovery source before verification
	// confirms the articles propagated. Retain originals; deferred cleanup after
	// successful verification is handled by the durable transfer lifecycle.
	if deleteOriginal && p.durableMode() {
		slog.InfoContext(ctx, "Durable mode: retaining original files until verification succeeds",
			"path", job.Path)
		deleteOriginal = false
	}
	if deleteOriginal {
		if p.deleteDelay > 0 {
			slog.InfoContext(ctx, "Waiting before deleting original files", "delay", p.deleteDelay)
			select {
			case <-time.After(p.deleteDelay):
			case <-ctx.Done():
				slog.WarnContext(ctx, "Context cancelled while waiting to delete files")
				return actualNzbPath, jobPostie, nil
			}
		}
		for _, fileInfo := range filesToProcess {
			if err := os.Remove(fileInfo.Path); err != nil {
				slog.WarnContext(ctx, "Could not delete original file", "path", fileInfo.Path, "error", err)
			}
		}
	}

	return actualNzbPath, jobPostie, nil
}

func (p *Processor) handleProcessingError(ctx context.Context, msg *goqite.Message, job *queue.FileJob, jobID string, err error) error {
	slog.ErrorContext(ctx, "Error processing file",
		"error", err.Error(),
		"errorType", fmt.Sprintf("%T", err),
		"path", job.Path,
		"retryCount", job.RetryCount,
		"maxRetries", maxRetries,
		"jobID", jobID,
	)

	job.RetryCount++

	if job.RetryCount >= maxRetries {
		fileName := getFileName(job.Path)
		slog.ErrorContext(ctx, "Job failed permanently after reaching max retries",
			"path", job.Path,
			"fileName", fileName,
			"retryCount", job.RetryCount,
			"error", err.Error(),
		)

		// Notify UI about the permanent failure
		if p.onJobError != nil {
			p.onJobError(fileName, err.Error())
		}

		if markErr := p.queue.MarkAsError(ctx, msg.ID, job, err.Error()); markErr != nil {
			slog.ErrorContext(ctx, "Failed to mark job as error", "error", markErr, "path", job.Path)
			// Re-add to queue as a fallback. Clear the in_progress row first so the
			// new goqite entry doesn't appear alongside a stale tracking row.
			if clearErr := p.queue.ClearInProgress(ctx, msg.ID); clearErr != nil {
				slog.WarnContext(ctx, "Failed to clear in-progress row before re-add", "error", clearErr, "path", job.Path)
			}
			if readdErr := p.queue.ReaddJob(ctx, job); readdErr != nil {
				slog.ErrorContext(ctx, "Failed to re-add job to queue", "error", readdErr, "path", job.Path)
			}
		}
		return nil
	}

	// Retry path: clear the in_progress tracking row for the old msg.ID before
	// re-adding, otherwise the same path is visible as two entries (the new
	// pending goqite row + the stale in_progress row keyed by the old ID).
	if clearErr := p.queue.ClearInProgress(ctx, msg.ID); clearErr != nil {
		slog.WarnContext(ctx, "Failed to clear in-progress row before retry", "error", clearErr, "path", job.Path)
	}
	if readdErr := p.queue.ReaddJob(ctx, job); readdErr != nil {
		slog.ErrorContext(ctx, "Failed to re-add job to queue for retry", "error", readdErr, "path", job.Path)
	}

	return nil
}

// CancelJob cancels a running job by its ID
func (p *Processor) CancelJob(jobID string) error {
	p.jobsMux.Lock()

	rj, exists := p.runningJobs[jobID]
	if !exists {
		p.jobsMux.Unlock()
		return fmt.Errorf("job %s is not currently running", jobID)
	}

	// Remove from tracking first to prevent duplicate events
	delete(p.runningJobs, jobID)

	p.jobsMux.Unlock()

	// Cancel the job's context
	rj.cancel()

	slog.Info("Job cancelled", "jobID", jobID)
	return nil
}

// GetRunningJobs returns a map of currently running job IDs
func (p *Processor) GetRunningJobs() map[string]bool {
	p.jobsMux.RLock()
	defer p.jobsMux.RUnlock()

	result := make(map[string]bool)
	for jobID := range p.runningJobs {
		result[jobID] = true
	}
	return result
}

// GetRunningJobItems returns detailed information about currently running jobs
func (p *Processor) GetRunningJobItems() []RunningJobItem {
	p.jobsMux.RLock()
	defer p.jobsMux.RUnlock()

	var items []RunningJobItem
	for jobID := range p.runningJobs {
		items = append(items, RunningJobItem{
			ID: jobID,
		})
	}
	return items
}

// GetRunningJobDetails returns detailed information about currently running jobs
func (p *Processor) GetRunningJobDetails() map[string]RunningJobDetails {
	p.jobsMux.RLock()
	defer p.jobsMux.RUnlock()

	details := make(map[string]RunningJobDetails)
	for jobID, jobDetail := range p.runningJobs {
		details[jobID] = RunningJobDetails{
			ID:       jobID,
			Path:     jobDetail.Path,
			FileName: jobDetail.FileName,
			Size:     jobDetail.Size,
			Progress: jobDetail.Progress.GetAllProgressState(),
		}
	}

	return details
}

// IsPathBeingProcessed checks if a file path is currently being processed.
// It checks both reserved paths (claimed but not yet in runningJobs) and running jobs.
// For folder mode (paths with FOLDER: prefix), it also checks if the path is within
// a folder that is being processed.
func (p *Processor) IsPathBeingProcessed(path string) bool {
	// Check reserved paths first (claimed from queue but not yet in runningJobs)
	p.reservedMux.RLock()
	for reservedPath := range p.reservedPaths {
		if reservedPath == path {
			p.reservedMux.RUnlock()
			return true
		}
		// Check if path is a file within a reserved folder
		if after, ok := strings.CutPrefix(reservedPath, "FOLDER:"); ok {
			folderPath := after
			if isWithinPath(path, folderPath) {
				p.reservedMux.RUnlock()
				return true
			}
		}
	}
	p.reservedMux.RUnlock()

	// Check running jobs
	p.jobsMux.RLock()
	defer p.jobsMux.RUnlock()

	for _, jobDetails := range p.runningJobs {
		if jobDetails.Path == path {
			return true
		}
		// Check if path is a file within a folder being processed
		if after, ok := strings.CutPrefix(jobDetails.Path, "FOLDER:"); ok {
			folderPath := after
			if isWithinPath(path, folderPath) {
				return true
			}
		}
	}
	return false
}

// reservePath marks a path as reserved (claimed from queue but not yet processing).
// This prevents race conditions where the watcher could re-add files during the gap
// between ReceiveFile() and processFile().
func (p *Processor) reservePath(path string) {
	p.reservedMux.Lock()
	defer p.reservedMux.Unlock()
	p.reservedPaths[path] = time.Now()
}

// unreservePath removes a path from the reserved set.
// Called when a job moves to runningJobs or fails before processing starts.
func (p *Processor) unreservePath(path string) {
	p.reservedMux.Lock()
	defer p.reservedMux.Unlock()
	delete(p.reservedPaths, path)
}

// PauseProcessing pauses the processor, preventing new jobs from starting and pausing active jobs
func (p *Processor) PauseProcessing() {
	p.pausedMux.Lock()
	defer p.pausedMux.Unlock()

	if !p.isPaused {
		p.isPaused = true

		// Pause all currently running jobs
		p.jobsMux.RLock()
		for jobID, job := range p.runningJobs {
			if job.pausableCtx != nil {
				job.pausableCtx.Pause()
				// Also set progress as paused
				if job.Progress != nil {
					job.Progress.SetAllPaused(true)
				}
				slog.Info("Paused running job", "jobID", jobID)
			}
		}
		p.jobsMux.RUnlock()

		slog.Info("Processor paused - new jobs blocked, active jobs suspended")
	}
}

// ResumeProcessing resumes the processor, allowing new jobs to start and resuming active jobs
func (p *Processor) ResumeProcessing() {
	p.pausedMux.Lock()
	defer p.pausedMux.Unlock()

	if p.isPaused {
		p.isPaused = false

		// Resume all currently running jobs
		p.jobsMux.RLock()
		for jobID, job := range p.runningJobs {
			if job.pausableCtx != nil {
				job.pausableCtx.Resume()
				// Also set progress as resumed
				if job.Progress != nil {
					job.Progress.SetAllPaused(false)
				}
				slog.Info("Resumed running job", "jobID", jobID)
			}
		}
		p.jobsMux.RUnlock()

		slog.Info("Processor resumed - new jobs allowed, active jobs resumed")
	}
}

// IsPaused returns whether the processor is currently paused
func (p *Processor) IsPaused() bool {
	p.pausedMux.RLock()
	defer p.pausedMux.RUnlock()
	return p.isPaused
}

func (p *Processor) Close() error {
	slog.Info("Processor shutdown initiated")

	// Set shutdown flag to prevent new operations
	// Note: Don't set poolManager to nil - it's a shared reference
	// and setting it to nil can affect other processors
	p.runningMux.Lock()
	p.isShuttingDown = true
	p.runningMux.Unlock()

	// Stop provider monitoring
	if p.providerCheckCancel != nil {
		p.providerCheckCancel()
	}

	// Get a snapshot of running jobs and cancel them
	p.jobsMux.Lock()
	runningJobsCount := len(p.runningJobs)

	// Cancel all running jobs
	for jobID, job := range p.runningJobs {
		job.cancel() // Cancel the job's context
		slog.Info("Cancelled running job", "jobID", jobID)
	}
	p.jobsMux.Unlock()

	if runningJobsCount == 0 {
		slog.Info("No running jobs to wait for")
		return nil
	}

	slog.Info("Waiting for running jobs to be cancelled", "count", runningJobsCount)

	// Wait for all jobs to complete with a timeout using WaitGroup
	timeout := 30 * time.Second
	done := make(chan struct{})

	go func() {
		defer close(done)
		p.jobsWg.Wait() // Wait for all jobs to call Done()
	}()

	// Wait for jobs to complete or timeout
	select {
	case <-done:
		slog.Info("All running jobs where cancelled successfully")
	case <-time.After(timeout):
		p.jobsMux.RLock()
		remainingJobs := len(p.runningJobs)
		p.jobsMux.RUnlock()

		slog.Warn("Timeout waiting for jobs to complete",
			"remainingJobs", remainingJobs,
			"timeout", timeout)
	}

	// Force clear any remaining jobs after timeout
	p.jobsMux.Lock()
	p.runningJobs = make(map[string]*RunningJob)
	p.jobsMux.Unlock()

	// Clear reserved paths
	p.reservedMux.Lock()
	p.reservedPaths = make(map[string]time.Time)
	p.reservedMux.Unlock()

	// Close the transfer runtime after all jobs have stopped. The shared NNTP
	// pool manager is owned elsewhere and intentionally not closed here.
	if p.transferRuntime != nil {
		if err := p.transferRuntime.Close(); err != nil {
			slog.Warn("Error closing transfer runtime", "error", err)
		}
	}

	slog.Info("Processor shutdown completed")
	return nil
}

// TransferRuntimeMetrics returns a snapshot of process-wide upload/PAR2
// resource usage for observability. Returns the zero value when no transfer
// runtime is configured.
func (p *Processor) TransferRuntimeMetrics() postie.RuntimeMetrics {
	return p.transferRuntime.Metrics()
}

// durableMode reports whether the durable verification path is active, in which
// case verification (and the destructive cleanup that depends on it) happens in
// the background service rather than inline at upload completion.
func (p *Processor) durableMode() bool {
	return p.transferRuntime.DurableVerificationEnabled()
}

// DurableMode reports whether the durable verification path is active. Exposed
// so callers (e.g. the backend) can skip starting the legacy post-check retry
// worker, which in durable mode would poll a permanently empty
// pending_article_checks table forever.
func (p *Processor) DurableMode() bool {
	return p != nil && p.durableMode()
}

// setAutoBlock enables or disables blocking of new jobs due to provider unavailability.
// Unlike PauseProcessing(), this does NOT pause jobs that are already running.
func (p *Processor) setAutoBlock(blocking bool) {
	p.autoPausedMux.Lock()
	defer p.autoPausedMux.Unlock()
	p.isAutoBlockingNewJobs = blocking
}

// IsAutoPaused returns true if the processor was automatically paused due to provider unavailability
func (p *Processor) IsAutoPaused() bool {
	p.autoPausedMux.RLock()
	defer p.autoPausedMux.RUnlock()
	return p.isAutoPaused
}

// GetAutoPauseReason returns the reason for automatic pause, if any
func (p *Processor) GetAutoPauseReason() string {
	p.autoPausedMux.RLock()
	defer p.autoPausedMux.RUnlock()
	return p.autoPauseReason
}

// monitorProviderAvailability monitors provider status and pauses/resumes processing accordingly
func (p *Processor) monitorProviderAvailability() {
	ticker := time.NewTicker(30 * time.Second) // Check every 30 seconds
	defer ticker.Stop()

	p.providerCheckTicker = ticker

	slog.Info("Started provider availability monitoring")

	for {
		select {
		case <-p.providerCheckCtx.Done():
			slog.Info("Provider monitoring stopped")
			return
		case <-ticker.C:
			p.checkAndHandleProviderAvailability()
		}
	}
}

// checkAndHandleProviderAvailability checks provider availability and pauses/resumes as needed
func (p *Processor) checkAndHandleProviderAvailability() {
	// Get pool manager reference safely
	p.runningMux.Lock()
	poolManager := p.poolManager
	p.runningMux.Unlock()

	if poolManager == nil {
		return
	}

	// Get pool metrics to check provider status
	metrics, err := poolManager.GetMetrics()
	if err != nil {
		slog.Error("Failed to get pool metrics for provider check", "error", err)
		return
	}

	// Count reachable providers. Error counts are cumulative, so they only carry
	// health information as a delta against the previous check — see
	// evaluateProviderAvailability.
	totalProviders := len(metrics.Providers)
	activeProviders, nextBaseline := evaluateProviderAvailability(metrics, p.providerErrorBaseline)
	p.providerErrorBaseline = nextBaseline

	p.autoPausedMux.Lock()
	wasAutoPaused := p.isAutoPaused
	p.autoPausedMux.Unlock()

	slog.Debug("Provider availability check",
		"activeProviders", activeProviders,
		"totalProviders", totalProviders,
		"wasAutoPaused", wasAutoPaused)

	// If no providers are available and we haven't auto-paused yet.
	// Use setAutoBlock instead of PauseProcessing so that already-running jobs
	// are not paused — they must be allowed to continue (and reconnect) so that
	// ActiveConnections eventually becomes > 0 and auto-resume can trigger.
	// Pausing running jobs creates a deadlock: paused jobs make no connections,
	// errors never clear, auto-resume never fires.
	if activeProviders == 0 && totalProviders > 0 && !wasAutoPaused {
		slog.Warn("No providers available - blocking new jobs (running jobs continue unaffected)")
		p.setAutoBlock(true)

		p.autoPausedMux.Lock()
		p.isAutoPaused = true
		p.autoPauseReason = "All NNTP providers are unavailable"
		p.autoPausedMux.Unlock()
	}

	// If providers are available and we were auto-paused, unblock new jobs.
	// Do not touch the manual pause state — if the user manually paused,
	// that remains in effect.
	if activeProviders > 0 && wasAutoPaused {
		slog.Info("Providers available - unblocking new jobs",
			"activeProviders", activeProviders)
		p.setAutoBlock(false)

		p.autoPausedMux.Lock()
		p.isAutoPaused = false
		p.autoPauseReason = ""
		p.autoPausedMux.Unlock()
	}
}

// collectFilesInFolder collects all files in the specified folder recursively
// It also calculates the relative path for each file, including the folder name
// e.g., if folderPath is "/home/user/MyFolder" and file is at "/home/user/MyFolder/sub/video.mp4"
// the RelativePath will be "MyFolder/sub/video.mp4"
func (p *Processor) collectFilesInFolder(folderPath string) ([]fileinfo.FileInfo, error) {
	var files []fileinfo.FileInfo

	// Get the parent directory to calculate relative paths that include the folder name
	parentDir := filepath.Dir(folderPath)

	err := filepath.Walk(folderPath, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}

		// Skip directories
		if info.IsDir() {
			return nil
		}

		// Check if the path is a symlink and skip if not following symlinks
		if !p.followSymlinks {
			isSymlinkFile, err := isSymlink(path)
			if err != nil {
				// Log error but continue processing other files
				return nil
			}
			if isSymlinkFile {
				// Skip symlinks
				return nil
			}
		}

		// Calculate relative path from parent directory (includes folder name)
		relativePath, relErr := filepath.Rel(parentDir, path)
		if relErr != nil {
			// Fallback to just the filename if relative path calculation fails
			relativePath = filepath.Base(path)
		}

		// Normalize path separators to forward slashes for cross-platform NZB compatibility
		relativePath = filepath.ToSlash(relativePath)

		// Add file to the list
		files = append(files, fileinfo.FileInfo{
			Path:         path,
			Size:         uint64(info.Size()),
			RelativePath: relativePath,
		})

		return nil
	})

	if err != nil {
		return nil, err
	}

	return files, nil
}

func getFileName(path string) string {
	// Simple filename extraction
	for i := len(path) - 1; i >= 0; i-- {
		if path[i] == '/' || path[i] == '\\' {
			return path[i+1:]
		}
	}
	return path
}
