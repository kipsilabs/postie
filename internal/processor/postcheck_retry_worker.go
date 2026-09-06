package processor

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"time"

	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/pool"
	"github.com/kipsilabs/postie/internal/queue"
)

// postCheckQueue is the subset of *queue.Queue used by PostCheckRetryWorker.
type postCheckQueue interface {
	GetArticlesForCheck(ctx context.Context, limit int) ([]queue.PendingArticleCheck, error)
	MarkArticleVerified(ctx context.Context, id int64) error
	MarkArticleCheckFailed(ctx context.Context, id int64) error
	UpdateArticleCheckRetry(ctx context.Context, id int64, retryCount int, nextRetryAt time.Time) error
	GetPendingCheckCountForItem(ctx context.Context, completedItemID string) (total int, pending int, failed int, err error)
	UpdateCompletedItemVerificationStatus(ctx context.Context, completedItemID string, status string) error
}

// PostCheckRetryWorker handles deferred article verification via STAT checks.
// When immediate post-check verification fails after all retries, articles are
// stored in the database and this worker periodically rechecks them with
// exponential backoff.
type PostCheckRetryWorker struct {
	queue           postCheckQueue
	checkPool       pool.NNTPClient
	cfg             config.PostCheck
	ctx             context.Context
	cancel          context.CancelFunc
	checkInterval   time.Duration
	initialDelay    time.Duration
	maxBackoff      time.Duration
	maxRetries      int
	batchSize       int
	statBatchSize   int
	onStatusChanged func() // notified when a completed item's verification status is updated
}

// NewPostCheckRetryWorker creates a new post check retry worker
func NewPostCheckRetryWorker(
	ctx context.Context,
	q postCheckQueue,
	checkPool pool.NNTPClient,
	cfg config.PostCheck,
	onStatusChanged func(),
) *PostCheckRetryWorker {
	workerCtx, cancel := context.WithCancel(ctx)

	checkInterval := cfg.DeferredCheckInterval.ToDuration()
	if checkInterval <= 0 {
		checkInterval = 2 * time.Minute
	}

	initialDelay := cfg.DeferredCheckDelay.ToDuration()
	if initialDelay <= 0 {
		initialDelay = 30 * time.Second
	}

	maxBackoff := cfg.DeferredMaxBackoff.ToDuration()
	if maxBackoff <= 0 {
		maxBackoff = 5 * time.Minute
	}

	maxRetries := cfg.DeferredMaxRetries
	if maxRetries <= 0 {
		maxRetries = 5
	}

	batchSize := cfg.DeferredBatchSize
	if batchSize <= 0 {
		batchSize = 10000
	}

	statBatchSize := cfg.StatBatchSize
	if statBatchSize <= 0 {
		statBatchSize = pool.DefaultStatBatchSize
	}

	return &PostCheckRetryWorker{
		queue:           q,
		checkPool:       checkPool,
		cfg:             cfg,
		ctx:             workerCtx,
		cancel:          cancel,
		checkInterval:   checkInterval,
		initialDelay:    initialDelay,
		maxBackoff:      maxBackoff,
		maxRetries:      maxRetries,
		batchSize:       batchSize,
		statBatchSize:   statBatchSize,
		onStatusChanged: onStatusChanged,
	}
}

// Start begins the retry worker loop
func (w *PostCheckRetryWorker) Start() {
	if w.cfg.Enabled == nil || !*w.cfg.Enabled {
		slog.Info("Post check retry worker not started (post check disabled)")
		return
	}

	slog.Info("Starting post check retry worker",
		"checkInterval", w.checkInterval,
		"initialDelay", w.initialDelay,
		"maxBackoff", w.maxBackoff,
		"maxRetries", w.maxRetries,
		"batchSize", w.batchSize)

	go w.run()
}

// Stop stops the retry worker
func (w *PostCheckRetryWorker) Stop() {
	slog.Info("Stopping post check retry worker")
	w.cancel()
}

// run is the main worker loop
func (w *PostCheckRetryWorker) run() {
	ticker := time.NewTicker(w.checkInterval)
	defer ticker.Stop()

	for {
		select {
		case <-w.ctx.Done():
			slog.Info("Post check retry worker stopped")
			return
		case <-ticker.C:
			for w.processRetries() {
				if w.ctx.Err() != nil {
					return
				}
			}
		}
	}
}

// processRetries checks for and processes pending article verifications.
// Returns true if a full batch was processed (more items may be pending).
func (w *PostCheckRetryWorker) processRetries() bool {
	ctx := w.ctx

	articles, err := w.queue.GetArticlesForCheck(ctx, w.batchSize)
	if err != nil {
		slog.ErrorContext(ctx, "Failed to get articles for deferred check", "error", err)
		return false
	}

	if len(articles) == 0 {
		return false
	}

	slog.InfoContext(ctx, "Processing deferred article checks", "count", len(articles))

	// Track completed items that need status updates
	completedItems := make(map[string]bool)

	// Parse groups up front so malformed rows are failed before the STAT sweep.
	checkable := make([]queue.PendingArticleCheck, 0, len(articles))
	for _, article := range articles {
		completedItems[article.CompletedItemID] = true

		var groups []string
		if err := json.Unmarshal([]byte(article.Groups), &groups); err != nil {
			slog.ErrorContext(ctx, "Failed to parse groups JSON", "error", err, "articleID", article.ID)
			if markErr := w.queue.MarkArticleCheckFailed(ctx, article.ID); markErr != nil {
				slog.ErrorContext(ctx, "Failed to mark article as failed", "error", markErr)
			}
			continue
		}
		checkable = append(checkable, article)
	}

	// Run batched STAT checks over all remaining articles.
	ids := make([]string, len(checkable))
	for i, article := range checkable {
		ids[i] = article.MessageID
	}
	missing, err := pool.StatMissing(ctx, w.checkPool, ids, w.statBatchSize)
	if err != nil {
		slog.WarnContext(ctx, "Deferred STAT sweep interrupted", "error", err)
		return false
	}

	for _, article := range checkable {
		if ctx.Err() != nil {
			return false
		}

		_, isMissing := missing[article.MessageID]

		if !isMissing {
			if err := w.queue.MarkArticleVerified(ctx, article.ID); err != nil {
				slog.ErrorContext(ctx, "Failed to mark article as verified", "error", err, "articleID", article.ID)
			} else {
				slog.DebugContext(ctx, "Article verified on deferred check",
					"messageID", article.MessageID, "retryCount", article.RetryCount)
			}
			continue
		}

		// Check if we should retry
		newRetryCount := article.RetryCount + 1
		if newRetryCount >= w.maxRetries {
			// Max retries exhausted - mark as failed
			if err := w.queue.MarkArticleCheckFailed(ctx, article.ID); err != nil {
				slog.ErrorContext(ctx, "Failed to mark article as failed", "error", err, "articleID", article.ID)
			}
			slog.WarnContext(ctx, "Article verification permanently failed",
				"messageID", article.MessageID,
				"retries", newRetryCount,
				"maxRetries", w.maxRetries)
			continue
		}

		// Schedule next retry with exponential backoff
		backoff := w.calculateBackoff(newRetryCount)
		nextRetry := time.Now().Add(backoff)

		if err := w.queue.UpdateArticleCheckRetry(ctx, article.ID, newRetryCount, nextRetry); err != nil {
			slog.ErrorContext(ctx, "Failed to update article check retry", "error", err, "articleID", article.ID)
		} else {
			slog.DebugContext(ctx, "Scheduled article recheck",
				"messageID", article.MessageID,
				"retryCount", newRetryCount,
				"nextRetry", nextRetry,
				"backoff", backoff)
		}
	}

	// Update completed item verification statuses
	for completedItemID := range completedItems {
		w.updateCompletedItemStatus(ctx, completedItemID)
	}

	// Notify listeners once per batch so per-item verification progress
	// advances in the UI even before items reach a final status.
	if w.onStatusChanged != nil {
		w.onStatusChanged()
	}

	return len(articles) == w.batchSize
}

// calculateBackoff calculates the exponential backoff delay
func (w *PostCheckRetryWorker) calculateBackoff(retryCount int) time.Duration {
	// Exponential backoff: initialDelay * 2^retryCount
	backoff := w.initialDelay * time.Duration(1<<retryCount)

	// Cap at max backoff
	if backoff > w.maxBackoff {
		return w.maxBackoff
	}

	return backoff
}

// updateCompletedItemStatus checks all articles for a completed item and updates its verification status
func (w *PostCheckRetryWorker) updateCompletedItemStatus(ctx context.Context, completedItemID string) {
	total, pending, failed, err := w.queue.GetPendingCheckCountForItem(ctx, completedItemID)
	if err != nil {
		slog.ErrorContext(ctx, "Failed to get pending check counts", "error", err, "completedItemID", completedItemID)
		return
	}

	// If there are still pending checks, don't update status yet
	if pending > 0 {
		return
	}

	// All checks are done - determine final status
	var status string
	if failed > 0 {
		status = "verification_failed"
		slog.WarnContext(ctx, "Completed item verification failed",
			"completedItemID", completedItemID,
			"total", total, "failed", failed)
	} else {
		status = "verified"
		slog.InfoContext(ctx, "All deferred articles verified successfully",
			"completedItemID", completedItemID, "total", total)
	}

	if err := w.queue.UpdateCompletedItemVerificationStatus(ctx, completedItemID, status); err != nil {
		slog.ErrorContext(ctx, "Failed to update completed item verification status",
			"error", err, "completedItemID", completedItemID)
	} else if w.onStatusChanged != nil {
		w.onStatusChanged()
	}
}

// GetFailureReason returns a human-readable reason for why retries stopped
func (w *PostCheckRetryWorker) GetFailureReason(retryCount int) string {
	if retryCount >= w.maxRetries {
		return fmt.Sprintf("exceeded max retries of %d", w.maxRetries)
	}
	return "unknown reason"
}
