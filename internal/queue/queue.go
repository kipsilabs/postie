package queue

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/javi11/postie/internal/database"
	_ "github.com/mattn/go-sqlite3"
	"maragu.dev/goqite"
)

var _ QueueInterface = (*Queue)(nil)

// genTransferID returns a new stable transfer identifier for a queue job.
func genTransferID() string {
	return uuid.NewString()
}

// PaginationParams defines parameters for paginated queries
type PaginationParams struct {
	Page   int    `json:"page"`   // 1-based page number
	Limit  int    `json:"limit"`  // Items per page
	SortBy string `json:"sortBy"` // Sort field: "created", "priority", "status", "filename", "size"
	Order  string `json:"order"`  // Sort order: "asc", "desc"
	Status string `json:"status"` // Status filter: "pending", "complete", "error", or "" for all
}

// PaginatedResult contains paginated queue items and metadata
type PaginatedResult struct {
	Items        []QueueItem `json:"items"`
	TotalItems   int         `json:"totalItems"`
	TotalPages   int         `json:"totalPages"`
	CurrentPage  int         `json:"currentPage"`
	ItemsPerPage int         `json:"itemsPerPage"`
	HasNext      bool        `json:"hasNext"`
	HasPrev      bool        `json:"hasPrev"`
}

// QueueInterface defines the interface for queue operations
type QueueInterface interface {
	AddFile(ctx context.Context, path string, size int64) error
	AddFileWithOptions(ctx context.Context, path string, size int64, opts AddOptions) error
	GetQueueItems(params PaginationParams) (*PaginatedResult, error)
	RemoveFromQueue(id string) error
	ClearQueue() error
	GetQueueStats() (map[string]any, error)
	SetQueueItemPriorityWithReorder(ctx context.Context, id string, newPriority int) error
	IsPathInQueue(path string) (bool, error)
	PendingTotalSize(ctx context.Context) (int64, error)
}

// AddOptions carries optional per-job fields when adding to the queue.
// Zero values preserve existing behavior; only the API entry point sets these today.
type AddOptions struct {
	Priority       int
	InputFolder    string // overrides processor's inferred root for relative-path → NZB output
	DeleteOriginal *bool  // overrides watcher.DeleteOriginalFile for this job only
}

type Queue struct {
	queue     *goqite.Queue
	db        *sql.DB
	runCtx    context.Context
	runCancel context.CancelFunc

	// addMu serializes check-then-insert in the duplicate-checking Add* methods.
	// goqite.Send uses its own *sql.DB handle so a SQL transaction cannot span
	// IsPathInQueue + Send; we close the TOCTOU window at the application level.
	// See issue #184: under MaxConcurrentUploads > 1 the watcher and processor
	// can both race to add the same path between the IsPathInQueue check and
	// the Send call.
	addMu sync.Mutex
}

type QueueItem struct {
	ID           string     `json:"id"`
	Path         string     `json:"path"`
	FileName     string     `json:"fileName"`
	Size         int64      `json:"size"`
	Status       string     `json:"status"`
	RetryCount   int        `json:"retryCount"`
	Priority     int        `json:"priority"`
	ErrorMessage *string    `json:"errorMessage"`
	CreatedAt    time.Time  `json:"createdAt"`
	UpdatedAt    time.Time  `json:"updatedAt"`
	CompletedAt  *time.Time `json:"completedAt"`
	NzbPath      *string    `json:"nzbPath"`
	// Script execution tracking fields
	ScriptStatus      *string    `json:"scriptStatus"` // null, completed, pending_retry, failed_permanent
	ScriptRetryCount  int        `json:"scriptRetryCount"`
	ScriptLastError   *string    `json:"scriptLastError"`
	ScriptNextRetryAt *time.Time `json:"scriptNextRetryAt"`
	// Verification status for completed items (null for non-completed)
	VerificationStatus *string `json:"verificationStatus"` // verified, pending_verification, verification_failed
	// Deferred verification progress (only set while pending_verification)
	VerifiedArticles *int `json:"verifiedArticles,omitempty"`
	TotalArticles    *int `json:"totalArticles,omitempty"`
}

type FileJob struct {
	Path       string    `json:"path"`
	Size       int64     `json:"size"`
	Priority   int       `json:"priority"`
	CreatedAt  time.Time `json:"createdAt"`
	RetryCount int       `json:"retryCount"`
	// TransferID is a stable identifier assigned when the job is first created.
	// It is carried through the persisted job body so it survives queue retries
	// and crash recovery, letting the durable upload architecture reuse the same
	// manifest and Message-IDs across attempts.
	TransferID string `json:"transferId,omitempty"`
	// InputFolder is the root directory used to derive the relative path
	// for NZB output. When empty, the processor falls back to its existing
	// inference (FOLDER: prefix → parent, watch folder match, or file dir).
	InputFolder string `json:"inputFolder,omitempty"`
	// DeleteOriginal optionally overrides the global delete-original setting
	// for this specific job. nil → use the processor's default.
	DeleteOriginal *bool `json:"deleteOriginal,omitempty"`
}

type CompletedItem struct {
	ID                   string     `json:"id"`
	Path                 string     `json:"path"`
	Size                 int64      `json:"size"`
	Priority             int        `json:"priority"`
	NzbPath              string     `json:"nzbPath"`
	CreatedAt            time.Time  `json:"createdAt"`
	CompletedAt          time.Time  `json:"completedAt"`
	JobData              []byte     `json:"jobData"`
	ScriptRetryCount     int        `json:"scriptRetryCount"`     // Number of script retry attempts
	ScriptFirstFailureAt *time.Time `json:"scriptFirstFailureAt"` // When the first script failure occurred
}

const (
	StatusPending  = "pending"
	StatusRunning  = "running"
	StatusComplete = "complete"
	StatusError    = "error"
)

func New(ctx context.Context, database *database.Database) (*Queue, error) {
	if database == nil {
		return nil, fmt.Errorf("database instance is required")
	}

	// Create goqite queue
	queue := goqite.New(goqite.NewOpts{
		DB:   database.DB,
		Name: "file_jobs",
	})

	runCtx, runCancel := context.WithCancel(ctx)

	q := &Queue{
		queue:     queue,
		db:        database.DB,
		runCtx:    runCtx,
		runCancel: runCancel,
	}

	// Recover any items that were in-progress when the app last crashed.
	// These items were deleted from goqite but not yet completed or errored.
	q.recoverInProgressItems(ctx)

	return q, nil
}

// DB returns the underlying database handle so adjacent stores (e.g. the
// durable transfer store) can share the same connection.
func (q *Queue) DB() *sql.DB {
	return q.db
}

// recoverInProgressItems re-queues any items that were being processed when the app crashed.
// These items exist in in_progress_items but not in goqite, completed_items, or errored_items.
func (q *Queue) recoverInProgressItems(ctx context.Context) {
	rows, err := q.db.QueryContext(ctx, `
		SELECT id, path, size, priority, created_at, job_data
		FROM in_progress_items
	`)
	if err != nil {
		slog.WarnContext(ctx, "Failed to query in-progress items for recovery", "error", err)
		return
	}

	// Phase 1: collect all rows into memory before closing the cursor.
	// With SetMaxOpenConns(1) the open rows cursor holds the single connection,
	// so any write (Send or DELETE) would deadlock if we tried it here.
	type item struct {
		id         string
		path       string
		size       int64
		priority   int64
		createdStr string
		jobData    []byte
	}

	var pending []item
	for rows.Next() {
		var it item
		if err := rows.Scan(&it.id, &it.path, &it.size, &it.priority, &it.createdStr, &it.jobData); err != nil {
			slog.WarnContext(ctx, "Failed to scan in-progress item", "error", err)
			continue
		}
		pending = append(pending, it)
	}
	_ = rows.Close() // release the read lock / connection before writing

	// Phase 2: now that the cursor is closed the connection is free for writes.
	var recovered int
	for _, it := range pending {
		// Re-insert into goqite so it will be processed again
		err := q.queue.Send(ctx, goqite.Message{
			Body:     it.jobData,
			Priority: int(it.priority),
		})
		if err != nil {
			slog.WarnContext(ctx, "Failed to re-queue in-progress item", "id", it.id, "path", it.path, "error", err)
			continue
		}

		// Remove from in_progress_items now that it is back in the queue
		_, _ = q.db.ExecContext(ctx, "DELETE FROM in_progress_items WHERE id = ?", it.id)
		recovered++
		slog.InfoContext(ctx, "Recovered in-progress item", "id", it.id, "path", it.path)
	}

	if recovered > 0 {
		slog.InfoContext(ctx, "Recovered items from previous crash", "count", recovered)
	}
}

// AddFile adds a file to the queue for processing
func (q *Queue) AddFile(ctx context.Context, path string, size int64) error {
	q.addMu.Lock()
	defer q.addMu.Unlock()

	// Check if the path already exists in pending queue, completed items, or errored items
	exists, err := q.IsPathInQueue(path)
	if err != nil {
		return fmt.Errorf("failed to check if path exists: %w", err)
	}
	if exists {
		slog.DebugContext(ctx, "File already exists in queue, ignoring", "path", path)
		return nil
	}

	job := FileJob{
		Path:       path,
		Size:       size,
		Priority:   0,
		CreatedAt:  time.Now().UTC(),
		RetryCount: 0,
		TransferID: genTransferID(),
	}

	jobData, err := json.Marshal(job)
	if err != nil {
		return fmt.Errorf("failed to marshal job: %w", err)
	}

	slog.InfoContext(ctx, "Adding file to queue", "path", path, "size", size)

	return q.queue.Send(ctx, goqite.Message{
		Body:     jobData,
		Priority: job.Priority,
	})
}

// AddManualFile adds a user-initiated (manual) upload to the queue. Manual
// uploads always preserve the original file regardless of the watcher's
// delete_original_file policy, so the per-job override is pinned to false.
// Automatic (watcher) uploads must not use this and instead leave the override
// unset so they continue to honour delete_original_file.
func (q *Queue) AddManualFile(ctx context.Context, path string, size int64) error {
	keepOriginal := false
	return q.AddFileWithOptions(ctx, path, size, AddOptions{DeleteOriginal: &keepOriginal})
}

// AddFileWithoutDuplicateCheck adds a file to the queue without checking for duplicates
// This is useful when original files are deleted after processing, making duplicate checks unnecessary
func (q *Queue) AddFileWithoutDuplicateCheck(ctx context.Context, path string, size int64) error {
	job := FileJob{
		Path:       path,
		Size:       size,
		Priority:   0,
		CreatedAt:  time.Now().UTC(),
		RetryCount: 0,
		TransferID: genTransferID(),
	}

	jobData, err := json.Marshal(job)
	if err != nil {
		return fmt.Errorf("failed to marshal job: %w", err)
	}

	slog.InfoContext(ctx, "Adding file to queue", "path", path, "size", size)

	return q.queue.Send(ctx, goqite.Message{
		Body:     jobData,
		Priority: job.Priority,
	})
}

// AddFileWithPriority adds a file to the queue with a specific priority
func (q *Queue) AddFileWithPriority(ctx context.Context, path string, size int64, priority int) error {
	q.addMu.Lock()
	defer q.addMu.Unlock()

	// Check if the path already exists in pending queue, completed items, or errored items
	exists, err := q.IsPathInQueue(path)
	if err != nil {
		return fmt.Errorf("failed to check if path exists: %w", err)
	}
	if exists {
		slog.DebugContext(ctx, "File already exists in queue, ignoring", "path", path)
		return nil
	}

	job := FileJob{
		Path:       path,
		Size:       size,
		Priority:   priority,
		CreatedAt:  time.Now().UTC(),
		RetryCount: 0,
		TransferID: genTransferID(),
	}

	jobData, err := json.Marshal(job)
	if err != nil {
		return fmt.Errorf("failed to marshal job: %w", err)
	}

	slog.InfoContext(ctx, "Adding file to queue", "path", path, "size", size)

	return q.queue.Send(ctx, goqite.Message{
		Body:     jobData,
		Priority: job.Priority,
	})
}

// AddFileWithPriorityWithoutDuplicateCheck adds a file to the queue with a specific priority without checking for duplicates
// This is useful when original files are deleted after processing, making duplicate checks unnecessary
func (q *Queue) AddFileWithPriorityWithoutDuplicateCheck(ctx context.Context, path string, size int64, priority int) error {
	job := FileJob{
		Path:       path,
		Size:       size,
		Priority:   priority,
		CreatedAt:  time.Now().UTC(),
		RetryCount: 0,
		TransferID: genTransferID(),
	}

	jobData, err := json.Marshal(job)
	if err != nil {
		return fmt.Errorf("failed to marshal job: %w", err)
	}

	slog.InfoContext(ctx, "Adding file to queue", "path", path, "size", size)

	return q.queue.Send(ctx, goqite.Message{
		Body:     jobData,
		Priority: job.Priority,
	})
}

// AddFileWithOptions adds a file to the queue with optional per-job overrides
// (priority, input folder for relative-path output, delete-original flag).
// Skips the add if the path is already tracked anywhere in the queue.
func (q *Queue) AddFileWithOptions(ctx context.Context, path string, size int64, opts AddOptions) error {
	q.addMu.Lock()
	defer q.addMu.Unlock()

	exists, err := q.IsPathInQueue(path)
	if err != nil {
		return fmt.Errorf("failed to check if path exists: %w", err)
	}
	if exists {
		slog.DebugContext(ctx, "File already exists in queue, ignoring", "path", path)
		return nil
	}

	job := FileJob{
		Path:           path,
		Size:           size,
		Priority:       opts.Priority,
		CreatedAt:      time.Now().UTC(),
		RetryCount:     0,
		TransferID:     genTransferID(),
		InputFolder:    opts.InputFolder,
		DeleteOriginal: opts.DeleteOriginal,
	}

	jobData, err := json.Marshal(job)
	if err != nil {
		return fmt.Errorf("failed to marshal job: %w", err)
	}

	slog.InfoContext(ctx, "Adding file to queue with options",
		"path", path, "size", size,
		"priority", opts.Priority,
		"inputFolder", opts.InputFolder,
		"hasDeleteOverride", opts.DeleteOriginal != nil,
	)

	return q.queue.Send(ctx, goqite.Message{
		Body:     jobData,
		Priority: job.Priority,
	})
}

// PendingTotalSize returns the sum of pending FileJob sizes still queued in
// goqite. Used by the processor to gate pickup behind queue.min_size_to_start.
func (q *Queue) PendingTotalSize(ctx context.Context) (int64, error) {
	var total sql.NullInt64
	err := q.db.QueryRowContext(ctx, `
		SELECT COALESCE(SUM(CAST(json_extract(body, '$.size') AS INTEGER)), 0)
		FROM goqite
		WHERE queue = 'file_jobs'
	`).Scan(&total)
	if err != nil {
		return 0, fmt.Errorf("failed to compute pending total size: %w", err)
	}
	return total.Int64, nil
}

// ReceiveFile gets the next file job from the queue and removes it immediately.
// The item is tracked in the in_progress_items table until CompleteFile or MarkAsError is called.
// On startup, RecoverInProgressItems restores any items that were lost due to a crash.
func (q *Queue) ReceiveFile(ctx context.Context) (*goqite.Message, *FileJob, error) {
	msg, err := q.queue.Receive(ctx)
	if err != nil {
		return nil, nil, err
	}

	// If no message available, return nil (goqite returns nil, nil when no messages)
	if msg == nil {
		return nil, nil, nil
	}

	var job FileJob
	if err := json.Unmarshal(msg.Body, &job); err != nil {
		// Delete the invalid message
		_ = q.queue.Delete(ctx, msg.ID)
		return nil, nil, fmt.Errorf("failed to unmarshal job: %w", err)
	}

	// Track the item as in-progress before deleting from goqite.
	// This ensures we can recover it if the app crashes before completion.
	created := job.CreatedAt.Format("2006-01-02T15:04:05.000Z")
	_, err = q.db.ExecContext(ctx, `
		INSERT OR REPLACE INTO in_progress_items (id, path, size, priority, created_at, job_data)
		VALUES (?, ?, ?, ?, ?, ?)
	`, string(msg.ID), job.Path, job.Size, job.Priority, created, msg.Body)
	if err != nil {
		// Non-fatal: log and continue — losing crash recovery is better than blocking the queue
		slog.WarnContext(ctx, "Failed to track in-progress item", "id", string(msg.ID), "error", err)
	}

	// Delete the message from the queue immediately when starting to process
	// If processing fails, we'll re-add it to the queue
	if err := q.queue.Delete(ctx, msg.ID); err != nil {
		// Clean up the in_progress entry we just added
		_, _ = q.db.ExecContext(ctx, "DELETE FROM in_progress_items WHERE id = ?", string(msg.ID))
		return nil, nil, fmt.Errorf("failed to remove job from queue: %w", err)
	}

	return msg, &job, nil
}

// CompleteFile marks a file job as completed and adds it to completed_items table
func (q *Queue) CompleteFile(ctx context.Context, msgID goqite.ID, nzbPath string, job *FileJob) error {
	// Insert into completed_items table
	created := job.CreatedAt.Format("2006-01-02T15:04:05.000Z")

	// Marshal job data for storage
	jobData, err := json.Marshal(job)
	if err != nil {
		return fmt.Errorf("failed to marshal job data: %w", err)
	}

	_, err = q.db.Exec(`
		INSERT INTO completed_items (id, path, size, priority, nzb_path, created_at, job_data)
		VALUES (?, ?, ?, ?, ?, ?, ?)
	`, string(msgID), job.Path, job.Size, job.Priority, nzbPath, created, jobData)

	if err != nil {
		return fmt.Errorf("failed to insert completed item: %w", err)
	}

	// Remove from in-progress tracking now that it is complete
	_, _ = q.db.ExecContext(ctx, "DELETE FROM in_progress_items WHERE id = ?", string(msgID))

	return nil
}

// CancelFile removes a cancelled job from in-progress tracking.
func (q *Queue) CancelFile(ctx context.Context, msgID goqite.ID) error {
	_, err := q.db.ExecContext(ctx, "DELETE FROM in_progress_items WHERE id = ?", string(msgID))
	return err
}

// ExtendTimeout extends the processing timeout for a file job
func (q *Queue) ExtendTimeout(ctx context.Context, msgID goqite.ID, duration time.Duration) error {
	return q.queue.Extend(ctx, msgID, duration)
}

// GetQueueItems returns paginated queue items with metadata
func (q *Queue) GetQueueItems(params PaginationParams) (*PaginatedResult, error) {
	// Validate and set defaults
	if params.Page < 1 {
		params.Page = 1
	}
	if params.Limit < 1 || params.Limit > 100 {
		params.Limit = 25 // Default page size
	}
	if params.SortBy == "" {
		params.SortBy = "created"
	}
	if params.Order == "" {
		params.Order = "desc"
	}

	// Calculate offset
	offset := (params.Page - 1) * params.Limit

	// Build ORDER BY clause
	orderBy := q.buildOrderByClause(params.SortBy, params.Order)

	var allItems []QueueItem
	var err error
	totalCount := 0

	// Handle status filtering
	switch params.Status {
	case "pending":
		// Only query pending items from goqite
		err = q.db.QueryRow("SELECT COUNT(*) FROM goqite WHERE queue = 'file_jobs'").Scan(&totalCount)
		if err != nil {
			return nil, fmt.Errorf("failed to get pending items count: %w", err)
		}
		if totalCount > 0 {
			allItems, err = q.getPendingItemsPaginated(orderBy, offset, params.Limit)
		}
	case "running":
		// Only query in-progress items
		err = q.db.QueryRow("SELECT COUNT(*) FROM in_progress_items").Scan(&totalCount)
		if err != nil {
			return nil, fmt.Errorf("failed to get in-progress items count: %w", err)
		}
		if totalCount > 0 {
			allItems, err = q.getInProgressItemsPaginated(orderBy, offset, params.Limit)
		}
	case "complete":
		// Only query completed items
		err = q.db.QueryRow("SELECT COUNT(*) FROM completed_items").Scan(&totalCount)
		if err != nil {
			return nil, fmt.Errorf("failed to get completed items count: %w", err)
		}
		if totalCount > 0 {
			allItems, err = q.getCompletedItemsPaginatedWithSort(orderBy, offset, params.Limit)
		}
	case "error":
		// Only query errored items
		err = q.db.QueryRow("SELECT COUNT(*) FROM errored_items").Scan(&totalCount)
		if err != nil {
			return nil, fmt.Errorf("failed to get errored items count: %w", err)
		}
		if totalCount > 0 {
			allItems, err = q.getErroredItemsPaginatedWithSort(orderBy, offset, params.Limit)
		}
	default:
		// No filter or "all" - query all tables
		var activeCount, runningCount, completedCount, erroredCount int

		err = q.db.QueryRow("SELECT COUNT(*) FROM goqite WHERE queue = 'file_jobs'").Scan(&activeCount)
		if err != nil {
			return nil, fmt.Errorf("failed to get active items count: %w", err)
		}

		err = q.db.QueryRow("SELECT COUNT(*) FROM in_progress_items").Scan(&runningCount)
		if err != nil {
			return nil, fmt.Errorf("failed to get in-progress items count: %w", err)
		}

		err = q.db.QueryRow("SELECT COUNT(*) FROM completed_items").Scan(&completedCount)
		if err != nil {
			return nil, fmt.Errorf("failed to get completed items count: %w", err)
		}

		err = q.db.QueryRow("SELECT COUNT(*) FROM errored_items").Scan(&erroredCount)
		if err != nil {
			return nil, fmt.Errorf("failed to get errored items count: %w", err)
		}

		totalCount = activeCount + runningCount + completedCount + erroredCount

		if totalCount > 0 {
			// Get items based on sort order and pagination
			switch params.SortBy {
			case "status":
				// When sorting by status, we need to merge results in order
				allItems, err = q.getMergedItemsByStatus(params.Order, offset, params.Limit)
			default:
				// For other sort fields, get from union query
				allItems, err = q.getMergedItemsPaginated(orderBy, offset, params.Limit)
			}
		}
	}

	if err != nil {
		return nil, err
	}

	// If no items, return empty result
	if totalCount == 0 {
		return &PaginatedResult{
			Items:        []QueueItem{},
			TotalItems:   0,
			TotalPages:   0,
			CurrentPage:  params.Page,
			ItemsPerPage: params.Limit,
			HasNext:      false,
			HasPrev:      false,
		}, nil
	}

	// Attach deferred verification progress to items awaiting verification
	q.attachVerificationProgress(allItems)

	// Calculate pagination metadata
	totalPages := (totalCount + params.Limit - 1) / params.Limit // Ceiling division

	result := &PaginatedResult{
		Items:        allItems,
		TotalItems:   totalCount,
		TotalPages:   totalPages,
		CurrentPage:  params.Page,
		ItemsPerPage: params.Limit,
		HasNext:      params.Page < totalPages,
		HasPrev:      params.Page > 1,
	}

	return result, nil
}

// attachVerificationProgress populates VerifiedArticles/TotalArticles for items
// currently awaiting verification. It aggregates transfer_files (durable upload
// path, article-count weighted per file) first, then falls back to
// pending_article_checks (legacy deferred path) for items without transfer rows.
func (q *Queue) attachVerificationProgress(items []QueueItem) {
	indexByID := make(map[string]int)
	for i := range items {
		if items[i].VerificationStatus != nil && *items[i].VerificationStatus == "pending_verification" {
			indexByID[items[i].ID] = i
		}
	}
	if len(indexByID) == 0 {
		return
	}

	// Durable upload path: per-file verification states weighted by article
	// count. Files mid-verification credit the articles that are not (or no
	// longer) recorded as open failures, so the bar advances as the
	// verification service resolves misses.
	covered := q.applyVerificationCounts(items, indexByID, `
		SELECT tf.completed_item_id,
		       SUM(tf.article_count) AS total,
		       SUM(CASE
		             WHEN tf.verification_state = 'verified' THEN tf.article_count
		             WHEN tf.verification_state IN ('verifying', 'verification_failed')
		               THEN MAX(tf.article_count - COALESCE(vf.open_count, 0), 0)
		             ELSE 0
		           END) AS verified
		FROM transfer_files tf
		LEFT JOIN (
			SELECT transfer_id, file_id, COUNT(*) AS open_count
			FROM verification_failures
			WHERE state IN ('pending', 'failed')
			GROUP BY transfer_id, file_id
		) vf ON vf.transfer_id = tf.transfer_id AND vf.file_id = tf.file_id
		WHERE tf.completed_item_id IN (%s)
		GROUP BY tf.completed_item_id
	`)
	for _, id := range covered {
		delete(indexByID, id)
	}
	if len(indexByID) == 0 {
		return
	}

	// Legacy deferred path: one row per article awaiting recheck.
	q.applyVerificationCounts(items, indexByID, `
		SELECT completed_item_id,
		       COUNT(*) AS total,
		       SUM(CASE WHEN status = 'verified' THEN 1 ELSE 0 END) AS verified
		FROM pending_article_checks
		WHERE completed_item_id IN (%s)
		GROUP BY completed_item_id
	`)
}

// applyVerificationCounts runs a grouped (id, total, verified) query template
// against the given item IDs and fills in progress on matching items.
// It returns the IDs that received counts.
func (q *Queue) applyVerificationCounts(items []QueueItem, indexByID map[string]int, queryTemplate string) []string {
	placeholders := make([]string, 0, len(indexByID))
	args := make([]any, 0, len(indexByID))
	for id := range indexByID {
		placeholders = append(placeholders, "?")
		args = append(args, id)
	}

	query := fmt.Sprintf(queryTemplate, strings.Join(placeholders, ","))

	rows, err := q.db.Query(query, args...)
	if err != nil {
		slog.Error("Failed to query verification progress", "error", err)
		return nil
	}
	defer func() { _ = rows.Close() }()

	var covered []string
	for rows.Next() {
		var id string
		var total, verified int
		if err := rows.Scan(&id, &total, &verified); err != nil {
			slog.Error("Failed to scan verification progress row", "error", err)
			return covered
		}
		if i, ok := indexByID[id]; ok && total > 0 {
			items[i].TotalArticles = &total
			items[i].VerifiedArticles = &verified
			covered = append(covered, id)
		}
	}
	if err := rows.Err(); err != nil {
		slog.Error("Failed to iterate verification progress rows", "error", err)
	}
	return covered
}

// buildOrderByClause constructs SQL ORDER BY clause
func (q *Queue) buildOrderByClause(sortBy, order string) string {
	var column string
	switch sortBy {
	case "created":
		column = "created_at"
	case "priority":
		column = "priority"
	case "filename":
		column = "file_name"
	case "size":
		column = "size"
	case "status":
		column = "status"
	default:
		column = "created_at"
	}

	direction := "DESC"
	if order == "asc" {
		direction = "ASC"
	}

	return fmt.Sprintf("%s %s", column, direction)
}

// getMergedItemsPaginated gets items from all sources with unified sorting
func (q *Queue) getMergedItemsPaginated(orderBy string, offset, limit int) ([]QueueItem, error) {
	// Union query to get all items with unified sorting
	query := fmt.Sprintf(`
		SELECT id, path, size, priority, status, retry_count, error_message,
		       created_at, updated_at, completed_at, nzb_path, file_name,
		       script_status, script_retry_count, script_last_error, script_next_retry_at,
		       verification_status
		FROM (
			-- Active queue items
			SELECT id,
				   json_extract(body, '$.path') as path,
				   json_extract(body, '$.size') as size,
				   json_extract(body, '$.priority') as priority,
				   'pending' as status,
				   json_extract(body, '$.retryCount') as retry_count,
				   NULL as error_message,
				   created as created_at,
				   updated as updated_at,
				   NULL as completed_at,
				   NULL as nzb_path,
				   json_extract(body, '$.path') as file_name,
				   NULL as script_status,
				   0 as script_retry_count,
				   NULL as script_last_error,
				   NULL as script_next_retry_at,
				   NULL as verification_status
			FROM goqite
			WHERE queue = 'file_jobs'

			UNION ALL

			-- In-progress items (currently being processed)
			SELECT id,
				   path as path,
				   size,
				   priority,
				   'running' as status,
				   json_extract(job_data, '$.retryCount') as retry_count,
				   NULL as error_message,
				   created_at,
				   started_at as updated_at,
				   NULL as completed_at,
				   NULL as nzb_path,
				   path as file_name,
				   NULL as script_status,
				   0 as script_retry_count,
				   NULL as script_last_error,
				   NULL as script_next_retry_at,
				   NULL as verification_status
			FROM in_progress_items

			UNION ALL

			-- Completed items
			SELECT id, path, size, priority, 'complete' as status,
				   0 as retry_count, NULL as error_message,
				   created_at, completed_at as updated_at, completed_at, nzb_path,
				   path as file_name,
				   script_status, script_retry_count, script_last_error, script_next_retry_at,
				   verification_status
			FROM completed_items

			UNION ALL

			-- Errored items
			SELECT id, path, size, priority, 'error' as status,
				   json_extract(job_data, '$.retryCount') as retry_count,
				   error_message, created_at, errored_at as updated_at,
				   NULL as completed_at, NULL as nzb_path,
				   path as file_name,
				   NULL as script_status,
				   0 as script_retry_count,
				   NULL as script_last_error,
				   NULL as script_next_retry_at,
				   NULL as verification_status
			FROM errored_items
		)
		ORDER BY %s
		LIMIT ? OFFSET ?`, orderBy)

	rows, err := q.db.Query(query, limit, offset)
	if err != nil {
		return nil, fmt.Errorf("failed to query paginated items: %w", err)
	}
	defer func() {
		_ = rows.Close()
	}()

	var items []QueueItem
	for rows.Next() {
		var item QueueItem
		var completedAtStr, nzbPathStr, errorMsgStr sql.NullString
		var scriptStatusStr, scriptLastErrorStr, scriptNextRetryAtStr sql.NullString
		var verificationStatusStr sql.NullString
		var createdAtStr, updatedAtStr string

		err := rows.Scan(
			&item.ID, &item.Path, &item.Size, &item.Priority, &item.Status,
			&item.RetryCount, &errorMsgStr, &createdAtStr, &updatedAtStr,
			&completedAtStr, &nzbPathStr, &item.FileName,
			&scriptStatusStr, &item.ScriptRetryCount, &scriptLastErrorStr, &scriptNextRetryAtStr,
			&verificationStatusStr,
		)
		if err != nil {
			continue // Skip invalid rows
		}

		// Parse timestamps
		if createdTime, err := time.Parse("2006-01-02T15:04:05.000Z", createdAtStr); err == nil {
			item.CreatedAt = createdTime
		}
		if updatedTime, err := time.Parse("2006-01-02T15:04:05.000Z", updatedAtStr); err == nil {
			item.UpdatedAt = updatedTime
		}
		if completedAtStr.Valid {
			if completedTime, err := time.Parse("2006-01-02T15:04:05.000Z", completedAtStr.String); err == nil {
				item.CompletedAt = &completedTime
			}
		}
		if scriptNextRetryAtStr.Valid {
			if nextRetryTime, err := time.Parse("2006-01-02T15:04:05.000Z", scriptNextRetryAtStr.String); err == nil {
				item.ScriptNextRetryAt = &nextRetryTime
			}
		}

		// Set optional fields
		if errorMsgStr.Valid {
			item.ErrorMessage = &errorMsgStr.String
		}
		if nzbPathStr.Valid {
			item.NzbPath = &nzbPathStr.String
		}
		if scriptStatusStr.Valid {
			item.ScriptStatus = &scriptStatusStr.String
		}
		if scriptLastErrorStr.Valid {
			item.ScriptLastError = &scriptLastErrorStr.String
		}
		if verificationStatusStr.Valid {
			item.VerificationStatus = &verificationStatusStr.String
		}

		// Extract filename from path if needed
		if item.FileName == "" {
			item.FileName = getFileName(item.Path)
		} else {
			item.FileName = getFileName(item.FileName)
		}

		items = append(items, item)
	}

	return items, rows.Err()
}

// getMergedItemsByStatus gets items sorted by status priority
func (q *Queue) getMergedItemsByStatus(order string, offset, limit int) ([]QueueItem, error) {
	// Define status priority order
	statusOrder := []string{"running", "pending", "error", "complete"}
	if order == "asc" {
		statusOrder = []string{"complete", "error", "pending", "running"}
	}

	var allItems []QueueItem
	remaining := limit
	currentOffset := offset

	for _, status := range statusOrder {
		if remaining <= 0 {
			break
		}

		// The offset must be consumed against the bucket's total size, not the
		// page it returns: a bucket smaller than the offset returns nothing,
		// and reducing by that empty page would skip items at the boundary.
		count, err := q.countItemsByStatus(status)
		if err != nil {
			return nil, err
		}
		if currentOffset >= count {
			currentOffset -= count
			continue
		}

		var items []QueueItem
		switch status {
		case "running":
			items, err = q.getInProgressItemsPaginated("started_at DESC", currentOffset, remaining)
		case "pending":
			items, err = q.getActiveItemsPaginated(currentOffset, remaining)
		case "complete":
			items, err = q.getCompletedItemsPaginated(currentOffset, remaining)
		case "error":
			items, err = q.getErroredItemsPaginated(currentOffset, remaining)
		}
		if err != nil {
			return nil, err
		}
		currentOffset = 0

		allItems = append(allItems, items...)
		remaining -= len(items)
	}

	return allItems, nil
}

// countItemsByStatus returns the number of items in the table backing status.
func (q *Queue) countItemsByStatus(status string) (int, error) {
	var query string
	switch status {
	case "running":
		query = "SELECT COUNT(*) FROM in_progress_items"
	case "pending":
		query = "SELECT COUNT(*) FROM goqite WHERE queue = 'file_jobs'"
	case "complete":
		query = "SELECT COUNT(*) FROM completed_items"
	case "error":
		query = "SELECT COUNT(*) FROM errored_items"
	default:
		return 0, fmt.Errorf("unknown status %q", status)
	}

	var count int
	if err := q.db.QueryRow(query).Scan(&count); err != nil {
		return 0, fmt.Errorf("failed to count %s items: %w", status, err)
	}
	return count, nil
}

// getInProgressItemsPaginated returns paginated in-progress items
func (q *Queue) getInProgressItemsPaginated(orderBy string, offset, limit int) ([]QueueItem, error) {
	query := fmt.Sprintf(`
		SELECT id, path, size, priority, created_at, started_at, job_data
		FROM in_progress_items
		ORDER BY %s
		LIMIT ? OFFSET ?`, orderBy)

	rows, err := q.db.Query(query, limit, offset)
	if err != nil {
		return nil, fmt.Errorf("failed to query in-progress items: %w", err)
	}
	defer func() { _ = rows.Close() }()

	var items []QueueItem
	for rows.Next() {
		var id, path, createdStr, startedStr string
		var size, priority int64
		var jobData []byte

		if err := rows.Scan(&id, &path, &size, &priority, &createdStr, &startedStr, &jobData); err != nil {
			continue
		}

		item := QueueItem{
			ID:       id,
			Path:     path,
			FileName: path,
			Size:     size,
			Priority: int(priority),
			Status:   StatusRunning,
		}

		if createdTime, err := time.Parse("2006-01-02T15:04:05.000Z", createdStr); err == nil {
			item.CreatedAt = createdTime
		}
		if startedTime, err := time.Parse("2006-01-02T15:04:05.000Z", startedStr); err == nil {
			item.UpdatedAt = startedTime
		}

		items = append(items, item)
	}

	return items, nil
}

// Helper methods for getting items from specific tables
func (q *Queue) getActiveItemsPaginated(offset, limit int) ([]QueueItem, error) {
	rows, err := q.db.Query(`
		SELECT id, created, updated, body
		FROM goqite 
		WHERE queue = 'file_jobs'
		ORDER BY priority DESC, created ASC
		LIMIT ? OFFSET ?`, limit, offset)
	if err != nil {
		return nil, err
	}
	defer func() {
		_ = rows.Close()
	}()

	var items []QueueItem
	for rows.Next() {
		var id, created, updated string
		var body []byte

		if err := rows.Scan(&id, &created, &updated, &body); err != nil {
			continue
		}

		var job FileJob
		if err := json.Unmarshal(body, &job); err != nil {
			continue
		}

		createdTime, _ := time.Parse("2006-01-02T15:04:05.000Z", created)
		updatedTime, _ := time.Parse("2006-01-02T15:04:05.000Z", updated)

		item := QueueItem{
			ID:         id,
			Path:       job.Path,
			FileName:   getFileName(job.Path),
			Size:       job.Size,
			Status:     StatusPending,
			RetryCount: job.RetryCount,
			Priority:   job.Priority,
			CreatedAt:  createdTime,
			UpdatedAt:  updatedTime,
		}

		items = append(items, item)
	}

	return items, rows.Err()
}

func (q *Queue) getCompletedItemsPaginated(offset, limit int) ([]QueueItem, error) {
	rows, err := q.db.Query(`
		SELECT id, path, size, priority, nzb_path, created_at, completed_at,
		       script_status, script_retry_count, script_last_error, script_next_retry_at,
		       verification_status
		FROM completed_items
		ORDER BY completed_at DESC
		LIMIT ? OFFSET ?`, limit, offset)
	if err != nil {
		return nil, err
	}
	defer func() {
		_ = rows.Close()
	}()

	var items []QueueItem
	for rows.Next() {
		var id, path, nzbPath, createdAt, completedAt string
		var scriptStatusStr, scriptLastErrorStr, scriptNextRetryAtStr sql.NullString
		var verificationStatusStr sql.NullString
		var size int64
		var priority, scriptRetryCount int

		if err := rows.Scan(&id, &path, &size, &priority, &nzbPath, &createdAt, &completedAt,
			&scriptStatusStr, &scriptRetryCount, &scriptLastErrorStr, &scriptNextRetryAtStr,
			&verificationStatusStr); err != nil {
			continue
		}

		createdTime, _ := time.Parse("2006-01-02T15:04:05.000Z", createdAt)
		completedTime, _ := time.Parse("2006-01-02T15:04:05.000Z", completedAt)

		item := QueueItem{
			ID:               id,
			Path:             path,
			FileName:         getFileName(path),
			Size:             size,
			Status:           StatusComplete,
			RetryCount:       0,
			Priority:         priority,
			CreatedAt:        createdTime,
			UpdatedAt:        completedTime,
			CompletedAt:      &completedTime,
			NzbPath:          &nzbPath,
			ScriptRetryCount: scriptRetryCount,
		}

		// Set optional script fields
		if scriptStatusStr.Valid {
			item.ScriptStatus = &scriptStatusStr.String
		}
		if scriptLastErrorStr.Valid {
			item.ScriptLastError = &scriptLastErrorStr.String
		}
		if scriptNextRetryAtStr.Valid {
			if nextRetryTime, err := time.Parse("2006-01-02T15:04:05.000Z", scriptNextRetryAtStr.String); err == nil {
				item.ScriptNextRetryAt = &nextRetryTime
			}
		}
		if verificationStatusStr.Valid {
			item.VerificationStatus = &verificationStatusStr.String
		}

		items = append(items, item)
	}

	return items, rows.Err()
}

func (q *Queue) getErroredItemsPaginated(offset, limit int) ([]QueueItem, error) {
	rows, err := q.db.Query(`
		SELECT id, path, size, priority, error_message, created_at, errored_at, job_data
		FROM errored_items
		ORDER BY errored_at DESC
		LIMIT ? OFFSET ?`, limit, offset)
	if err != nil {
		return nil, err
	}
	defer func() {
		_ = rows.Close()
	}()

	var items []QueueItem
	for rows.Next() {
		var id, path, errMsg, createdAt, erroredAt string
		var size int64
		var priority int
		var jobData []byte

		if err := rows.Scan(&id, &path, &size, &priority, &errMsg, &createdAt, &erroredAt, &jobData); err != nil {
			continue
		}

		var job FileJob
		if err := json.Unmarshal(jobData, &job); err != nil {
			continue
		}

		createdTime, _ := time.Parse("2006-01-02T15:04:05.000Z", createdAt)
		erroredTime, _ := time.Parse("2006-01-02T15:04:05.000Z", erroredAt)

		item := QueueItem{
			ID:           id,
			Path:         path,
			FileName:     getFileName(path),
			Size:         size,
			Status:       StatusError,
			RetryCount:   job.RetryCount,
			Priority:     priority,
			ErrorMessage: &errMsg,
			CreatedAt:    createdTime,
			UpdatedAt:    erroredTime,
		}

		items = append(items, item)
	}

	return items, rows.Err()
}

// getPendingItemsPaginated gets pending items with custom sorting
func (q *Queue) getPendingItemsPaginated(orderBy string, offset, limit int) ([]QueueItem, error) {
	// Map column names for goqite table (which uses JSON body)
	var sortColumn string
	switch orderBy {
	case "created_at DESC":
		sortColumn = "created DESC"
	case "created_at ASC":
		sortColumn = "created ASC"
	case "size DESC":
		sortColumn = "CAST(json_extract(body, '$.size') AS INTEGER) DESC"
	case "size ASC":
		sortColumn = "CAST(json_extract(body, '$.size') AS INTEGER) ASC"
	case "priority DESC":
		sortColumn = "CAST(json_extract(body, '$.priority') AS INTEGER) DESC"
	case "priority ASC":
		sortColumn = "CAST(json_extract(body, '$.priority') AS INTEGER) ASC"
	case "file_name DESC":
		sortColumn = "json_extract(body, '$.path') DESC"
	case "file_name ASC":
		sortColumn = "json_extract(body, '$.path') ASC"
	default:
		sortColumn = "created DESC"
	}

	query := fmt.Sprintf(`
		SELECT id, created, updated, body
		FROM goqite
		WHERE queue = 'file_jobs'
		ORDER BY %s
		LIMIT ? OFFSET ?`, sortColumn)

	rows, err := q.db.Query(query, limit, offset)
	if err != nil {
		return nil, err
	}
	defer func() {
		_ = rows.Close()
	}()

	var items []QueueItem
	for rows.Next() {
		var id, created, updated string
		var body []byte

		if err := rows.Scan(&id, &created, &updated, &body); err != nil {
			continue
		}

		var job FileJob
		if err := json.Unmarshal(body, &job); err != nil {
			continue
		}

		createdTime, _ := time.Parse("2006-01-02T15:04:05.000Z", created)
		updatedTime, _ := time.Parse("2006-01-02T15:04:05.000Z", updated)

		item := QueueItem{
			ID:         id,
			Path:       job.Path,
			FileName:   getFileName(job.Path),
			Size:       job.Size,
			Status:     StatusPending,
			RetryCount: job.RetryCount,
			Priority:   job.Priority,
			CreatedAt:  createdTime,
			UpdatedAt:  updatedTime,
		}

		items = append(items, item)
	}

	return items, rows.Err()
}

// getCompletedItemsPaginatedWithSort gets completed items with custom sorting
func (q *Queue) getCompletedItemsPaginatedWithSort(orderBy string, offset, limit int) ([]QueueItem, error) {
	query := fmt.Sprintf(`
		SELECT id, path, size, priority, nzb_path, created_at, completed_at,
		       script_status, script_retry_count, script_last_error, script_next_retry_at,
		       verification_status
		FROM completed_items
		ORDER BY %s
		LIMIT ? OFFSET ?`, orderBy)

	rows, err := q.db.Query(query, limit, offset)
	if err != nil {
		return nil, err
	}
	defer func() {
		_ = rows.Close()
	}()

	var items []QueueItem
	for rows.Next() {
		var id, path, nzbPath, createdAt, completedAt string
		var scriptStatusStr, scriptLastErrorStr, scriptNextRetryAtStr sql.NullString
		var verificationStatusStr sql.NullString
		var size int64
		var priority, scriptRetryCount int

		if err := rows.Scan(&id, &path, &size, &priority, &nzbPath, &createdAt, &completedAt,
			&scriptStatusStr, &scriptRetryCount, &scriptLastErrorStr, &scriptNextRetryAtStr,
			&verificationStatusStr); err != nil {
			continue
		}

		createdTime, _ := time.Parse("2006-01-02T15:04:05.000Z", createdAt)
		completedTime, _ := time.Parse("2006-01-02T15:04:05.000Z", completedAt)

		item := QueueItem{
			ID:               id,
			Path:             path,
			FileName:         getFileName(path),
			Size:             size,
			Status:           StatusComplete,
			RetryCount:       0,
			Priority:         priority,
			CreatedAt:        createdTime,
			UpdatedAt:        completedTime,
			CompletedAt:      &completedTime,
			NzbPath:          &nzbPath,
			ScriptRetryCount: scriptRetryCount,
		}

		if scriptStatusStr.Valid {
			item.ScriptStatus = &scriptStatusStr.String
		}
		if scriptLastErrorStr.Valid {
			item.ScriptLastError = &scriptLastErrorStr.String
		}
		if scriptNextRetryAtStr.Valid {
			if nextRetryTime, err := time.Parse("2006-01-02T15:04:05.000Z", scriptNextRetryAtStr.String); err == nil {
				item.ScriptNextRetryAt = &nextRetryTime
			}
		}
		if verificationStatusStr.Valid {
			item.VerificationStatus = &verificationStatusStr.String
		}

		items = append(items, item)
	}

	return items, rows.Err()
}

// getErroredItemsPaginatedWithSort gets errored items with custom sorting
func (q *Queue) getErroredItemsPaginatedWithSort(orderBy string, offset, limit int) ([]QueueItem, error) {
	query := fmt.Sprintf(`
		SELECT id, path, size, priority, error_message, created_at, errored_at, job_data
		FROM errored_items
		ORDER BY %s
		LIMIT ? OFFSET ?`, orderBy)

	rows, err := q.db.Query(query, limit, offset)
	if err != nil {
		return nil, err
	}
	defer func() {
		_ = rows.Close()
	}()

	var items []QueueItem
	for rows.Next() {
		var id, path, errMsg, createdAt, erroredAt string
		var size int64
		var priority int
		var jobData []byte

		if err := rows.Scan(&id, &path, &size, &priority, &errMsg, &createdAt, &erroredAt, &jobData); err != nil {
			continue
		}

		var job FileJob
		if err := json.Unmarshal(jobData, &job); err != nil {
			continue
		}

		createdTime, _ := time.Parse("2006-01-02T15:04:05.000Z", createdAt)
		erroredTime, _ := time.Parse("2006-01-02T15:04:05.000Z", erroredAt)

		item := QueueItem{
			ID:           id,
			Path:         path,
			FileName:     getFileName(path),
			Size:         size,
			Status:       StatusError,
			RetryCount:   job.RetryCount,
			Priority:     priority,
			ErrorMessage: &errMsg,
			CreatedAt:    createdTime,
			UpdatedAt:    erroredTime,
		}

		items = append(items, item)
	}

	return items, rows.Err()
}

// RemoveFromQueue removes an item from the queue by ID (handles active, completed, and errored items)
func (q *Queue) RemoveFromQueue(id string) error {
	// First check if the item exists in completed items
	var exists bool
	err := q.db.QueryRow("SELECT EXISTS(SELECT 1 FROM completed_items WHERE id = ?)", id).Scan(&exists)
	if err != nil {
		return fmt.Errorf("failed to check completed items: %w", err)
	}

	if exists {
		// Remove from completed items
		return q.RemoveCompletedItem(id)
	}

	// Check if the item exists in errored items
	err = q.db.QueryRow("SELECT EXISTS(SELECT 1 FROM errored_items WHERE id = ?)", id).Scan(&exists)
	if err != nil {
		return fmt.Errorf("failed to check errored items: %w", err)
	}

	if exists {
		// Remove from errored items
		return q.RemoveErroredItem(id)
	}

	// Check if the item is in-progress (delete from tracking table; job will be lost)
	err = q.db.QueryRow("SELECT EXISTS(SELECT 1 FROM in_progress_items WHERE id = ?)", id).Scan(&exists)
	if err != nil {
		return fmt.Errorf("failed to check in-progress items: %w", err)
	}

	if exists {
		_, err = q.db.Exec("DELETE FROM in_progress_items WHERE id = ?", id)
		if err != nil {
			return fmt.Errorf("failed to remove in-progress item: %w", err)
		}
		return nil
	}

	// If not found in completed or errored items, try to remove from active queue
	return q.queue.Delete(q.runCtx, goqite.ID(id))
}

// RemoveCompletedItem removes a completed item and its associated NZB file
func (q *Queue) RemoveCompletedItem(id string) error {
	// Get the NZB path before deleting
	var nzbPath string
	err := q.db.QueryRow("SELECT nzb_path FROM completed_items WHERE id = ?", id).Scan(&nzbPath)
	if err != nil {
		if err == sql.ErrNoRows {
			return fmt.Errorf("completed item not found: %s", id)
		}
		return fmt.Errorf("failed to get NZB path: %w", err)
	}

	// Delete dependent article checks first (FK constraint)
	_, err = q.db.Exec("DELETE FROM pending_article_checks WHERE completed_item_id = ?", id)
	if err != nil {
		return fmt.Errorf("failed to delete pending article checks: %w", err)
	}

	// Delete the database record
	_, err = q.db.Exec("DELETE FROM completed_items WHERE id = ?", id)
	if err != nil {
		return fmt.Errorf("failed to delete completed item: %w", err)
	}

	// Delete the NZB file
	if err := os.Remove(nzbPath); err != nil && !os.IsNotExist(err) {
		slog.Warn("Failed to delete NZB file", "path", nzbPath, "error", err)
		// Don't return error here as the database record is already deleted
	}

	return nil
}

// RemoveErroredItem removes an errored item from the database
func (q *Queue) RemoveErroredItem(id string) error {
	// Delete the database record
	_, err := q.db.Exec("DELETE FROM errored_items WHERE id = ?", id)
	if err != nil {
		return fmt.Errorf("failed to delete errored item: %w", err)
	}

	return nil
}

// ClearQueue removes all completed, errored, and active items from the queue
func (q *Queue) ClearQueue() error {
	// Get all NZB paths before deleting
	rows, err := q.db.Query("SELECT nzb_path FROM completed_items")
	if err != nil {
		return err
	}
	defer func() {
		if err := rows.Close(); err != nil {
			slog.Error("Failed to close rows", "error", err)
		}
	}()

	var nzbPaths []string
	for rows.Next() {
		var nzbPath string
		if err := rows.Scan(&nzbPath); err != nil {
			continue
		}
		nzbPaths = append(nzbPaths, nzbPath)
	}

	// Clear pending article checks first (FK constraint)
	_, err = q.db.Exec("DELETE FROM pending_article_checks")
	if err != nil {
		return err
	}

	// Clear completed items from database
	_, err = q.db.Exec("DELETE FROM completed_items")
	if err != nil {
		return err
	}

	// Clear errored items from database
	_, err = q.db.Exec("DELETE FROM errored_items")
	if err != nil {
		return err
	}

	// Delete all NZB files
	for _, nzbPath := range nzbPaths {
		if err := os.Remove(nzbPath); err != nil && !os.IsNotExist(err) {
			slog.Warn("Failed to delete NZB file during clear", "path", nzbPath, "error", err)
		}
	}

	// Also clear active queue items
	_, err = q.db.Exec("DELETE FROM goqite WHERE queue = 'file_jobs'")
	return err
}

// ClearCompletedItems removes only completed items and their NZB files
func (q *Queue) ClearCompletedItems() error {
	// Get all NZB paths before deleting
	rows, err := q.db.Query("SELECT nzb_path FROM completed_items")
	if err != nil {
		return err
	}
	defer func() {
		if err := rows.Close(); err != nil {
			slog.Error("Failed to close rows", "error", err)
		}
	}()

	var nzbPaths []string
	for rows.Next() {
		var nzbPath string
		if err := rows.Scan(&nzbPath); err != nil {
			continue
		}
		nzbPaths = append(nzbPaths, nzbPath)
	}

	// Clear pending article checks first (FK constraint)
	_, err = q.db.Exec("DELETE FROM pending_article_checks")
	if err != nil {
		return err
	}

	// Clear completed items from database
	_, err = q.db.Exec("DELETE FROM completed_items")
	if err != nil {
		return err
	}

	// Delete all NZB files
	for _, nzbPath := range nzbPaths {
		if err := os.Remove(nzbPath); err != nil && !os.IsNotExist(err) {
			slog.Warn("Failed to delete NZB file", "path", nzbPath, "error", err)
		}
	}

	return nil
}

// GetQueueStats returns statistics about the queue including completed and errored items
func (q *Queue) GetQueueStats() (map[string]any, error) {
	stats := make(map[string]any)

	// Get total pending count (all items in queue are pending)
	var pending int
	err := q.db.QueryRow("SELECT COUNT(*) FROM goqite WHERE queue = 'file_jobs'").Scan(&pending)
	if err != nil {
		return nil, err
	}
	stats["pending"] = pending
	stats["total"] = pending // updated below to include running

	// Count items currently being processed
	var running int
	err = q.db.QueryRow("SELECT COUNT(*) FROM in_progress_items").Scan(&running)
	if err == nil {
		stats["running"] = running
	} else {
		stats["running"] = 0
	}

	// Count completed items
	var complete int
	err = q.db.QueryRow("SELECT COUNT(*) FROM completed_items").Scan(&complete)
	if err == nil {
		stats["complete"] = complete
	} else {
		stats["complete"] = 0
	}

	// Count errored items
	var errorCount int
	err = q.db.QueryRow("SELECT COUNT(*) FROM errored_items").Scan(&errorCount)
	if err == nil {
		stats["error"] = errorCount
	} else {
		stats["error"] = 0
	}

	// Count completed items by durable verification status (issue #184).
	var pendingVerification int
	if err := q.db.QueryRow("SELECT COUNT(*) FROM completed_items WHERE verification_status = 'pending_verification'").Scan(&pendingVerification); err == nil {
		stats["pendingVerification"] = pendingVerification
	} else {
		stats["pendingVerification"] = 0
	}

	var verificationFailed int
	if err := q.db.QueryRow("SELECT COUNT(*) FROM completed_items WHERE verification_status = 'verification_failed'").Scan(&verificationFailed); err == nil {
		stats["verificationFailed"] = verificationFailed
	} else {
		stats["verificationFailed"] = 0
	}

	// Update total to include running items
	if r, ok := stats["running"].(int); ok {
		stats["total"] = pending + r
	}

	// Add total including completed and errored
	stats["total_including_completed"] = pending + running + complete + errorCount

	return stats, nil
}

// Close closes the queue context (database is managed externally)
func (q *Queue) Close() error {
	if q.runCancel != nil {
		q.runCancel()
	}
	return nil
}

// IsPathInQueue checks if a file path already exists in pending queue, completed items, or errored items
func (q *Queue) IsPathInQueue(path string) (bool, error) {
	// Check pending queue items
	var count int
	err := q.db.QueryRow(`
		SELECT COUNT(*) FROM goqite 
		WHERE queue = 'file_jobs' AND json_extract(body, '$.path') = ?
	`, path).Scan(&count)
	if err != nil {
		return false, fmt.Errorf("failed to check pending queue: %w", err)
	}
	if count > 0 {
		return true, nil
	}

	// Check completed items
	err = q.db.QueryRow("SELECT COUNT(*) FROM completed_items WHERE path = ?", path).Scan(&count)
	if err != nil {
		return false, fmt.Errorf("failed to check completed items: %w", err)
	}
	if count > 0 {
		return true, nil
	}

	// Check in-progress items
	err = q.db.QueryRow("SELECT COUNT(*) FROM in_progress_items WHERE path = ?", path).Scan(&count)
	if err != nil {
		return false, fmt.Errorf("failed to check in-progress items: %w", err)
	}
	if count > 0 {
		return true, nil
	}

	// Check errored items
	err = q.db.QueryRow("SELECT COUNT(*) FROM errored_items WHERE path = ?", path).Scan(&count)
	if err != nil {
		return false, fmt.Errorf("failed to check errored items: %w", err)
	}
	if count > 0 {
		return true, nil
	}

	return false, nil
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

// GetCompletedItemNzbPath returns the NZB path for a completed item
func (q *Queue) GetCompletedItemNzbPath(id string) (string, error) {
	var nzbPath string
	err := q.db.QueryRow("SELECT nzb_path FROM completed_items WHERE id = ?", id).Scan(&nzbPath)
	if err != nil {
		if err == sql.ErrNoRows {
			return "", fmt.Errorf("completed item not found: %s", id)
		}
		return "", fmt.Errorf("failed to get NZB path: %w", err)
	}
	return nzbPath, nil
}

// DebugQueueItem returns debug information about a specific queue item
func (q *Queue) DebugQueueItem(id string) (map[string]any, error) {
	var received int
	var timeout, created, updated string
	var body []byte

	err := q.db.QueryRow(`
		SELECT received, timeout, created, updated, body
		FROM goqite 
		WHERE id = ? AND queue = 'file_jobs'
	`, id).Scan(&received, &timeout, &created, &updated, &body)

	if err != nil {
		if err == sql.ErrNoRows {
			return nil, fmt.Errorf("job not found: %s", id)
		}
		return nil, fmt.Errorf("failed to get job debug info: %w", err)
	}

	// Parse timeout
	timeoutTime, timeoutErr := time.Parse("2006-01-02T15:04:05.000Z", timeout)
	now := time.Now()

	// Check processor status
	debug := map[string]any{
		"id":                id,
		"received":          received,
		"timeout":           timeout,
		"created":           created,
		"updated":           updated,
		"currentTime":       now.Format("2006-01-02T15:04:05.000Z"),
		"timeoutParseError": timeoutErr,
		"isBeforeTimeout":   timeoutErr == nil && now.Before(timeoutTime),
		"shouldBeRunning":   timeoutErr == nil && now.Before(timeoutTime) && received > 0,
	}

	if timeoutErr == nil {
		debug["timeoutDiff"] = timeoutTime.Sub(now).String()
	}

	return debug, nil
}

// ReaddJob re-adds a job to the queue (used when processing fails)
func (q *Queue) ReaddJob(ctx context.Context, job *FileJob) error {
	jobData, err := json.Marshal(job)
	if err != nil {
		return fmt.Errorf("failed to marshal job: %w", err)
	}

	return q.queue.Send(ctx, goqite.Message{
		Body:     jobData,
		Priority: job.Priority,
	})
}

// ClearInProgress removes the in-progress tracking row for the given message ID.
// Call this when a retry is about to be re-queued under a new ID, otherwise the
// old in_progress row leaks and the same path appears as duplicate UI entries
// across the pending goqite row and the stale in_progress row.
func (q *Queue) ClearInProgress(ctx context.Context, msgID goqite.ID) error {
	_, err := q.db.ExecContext(ctx, "DELETE FROM in_progress_items WHERE id = ?", string(msgID))
	if err != nil {
		return fmt.Errorf("failed to clear in-progress item %s: %w", string(msgID), err)
	}
	return nil
}

// SetQueueItemPriority updates the priority of a pending queue item by id
func (q *Queue) SetQueueItemPriority(id string, priority int) error {
	// Get the job body for the given id
	var body []byte
	err := q.db.QueryRow("SELECT body FROM goqite WHERE id = ? AND queue = 'file_jobs'", id).Scan(&body)
	if err != nil {
		if err == sql.ErrNoRows {
			return fmt.Errorf("pending queue item not found: %s", id)
		}
		return fmt.Errorf("failed to get queue item: %w", err)
	}

	// Unmarshal, update priority, marshal
	var job FileJob
	if err := json.Unmarshal(body, &job); err != nil {
		return fmt.Errorf("failed to unmarshal job: %w", err)
	}
	job.Priority = priority
	newBody, err := json.Marshal(job)
	if err != nil {
		return fmt.Errorf("failed to marshal updated job: %w", err)
	}

	// Update the body in the database
	_, err = q.db.Exec("UPDATE goqite SET body = ?, updated = strftime('%Y-%m-%dT%H:%M:%fZ') WHERE id = ? AND queue = 'file_jobs'", newBody, id)
	if err != nil {
		return fmt.Errorf("failed to update queue item priority: %w", err)
	}
	return nil
}

// SetQueueItemPriorityWithReorder updates the priority of a pending queue item using goqite's native priority
// Higher priority numbers (0, 1, 2, ...) are processed first
func (q *Queue) SetQueueItemPriorityWithReorder(ctx context.Context, id string, newPriority int) error {
	// Get the current job data to update it
	var body []byte
	err := q.db.QueryRow("SELECT body FROM goqite WHERE id = ? AND queue = 'file_jobs'", id).Scan(&body)
	if err != nil {
		if err == sql.ErrNoRows {
			return fmt.Errorf("pending queue item not found: %s", id)
		}
		return fmt.Errorf("failed to get queue item: %w", err)
	}

	// Unmarshal and update priority in job data
	var job FileJob
	if err := json.Unmarshal(body, &job); err != nil {
		return fmt.Errorf("failed to unmarshal job: %w", err)
	}

	job.Priority = newPriority
	newBody, err := json.Marshal(job)
	if err != nil {
		return fmt.Errorf("failed to marshal updated job: %w", err)
	}

	// Update both the body and the goqite priority field
	_, err = q.db.Exec(`
		UPDATE goqite 
		SET body = ?, priority = ?, updated = strftime('%Y-%m-%dT%H:%M:%fZ') 
		WHERE id = ? AND queue = 'file_jobs'
	`, newBody, newPriority, id)

	if err != nil {
		return fmt.Errorf("failed to update queue item priority: %w", err)
	}

	return nil
}

// MarkAsError marks a file job as errored and adds it to the errored_items table
func (q *Queue) MarkAsError(ctx context.Context, msgID goqite.ID, job *FileJob, errMsg string) error {
	created := job.CreatedAt.Format("2006-01-02T15:04:05.000Z")

	jobData, err := json.Marshal(job)
	if err != nil {
		return fmt.Errorf("failed to marshal job data: %w", err)
	}

	_, err = q.db.Exec(`
		INSERT INTO errored_items (id, path, size, priority, error_message, created_at, job_data)
		VALUES (?, ?, ?, ?, ?, ?, ?)
	`, string(msgID), job.Path, job.Size, job.Priority, errMsg, created, jobData)

	if err != nil {
		return fmt.Errorf("failed to insert errored item: %w", err)
	}

	// Remove from in-progress tracking now that it is recorded as an error
	_, _ = q.db.ExecContext(ctx, "DELETE FROM in_progress_items WHERE id = ?", string(msgID))

	return nil
}

// RetryErroredJob retries an errored job by moving it back to the main queue
func (q *Queue) RetryErroredJob(ctx context.Context, id string) error {
	var jobData []byte
	err := q.db.QueryRow("SELECT job_data FROM errored_items WHERE id = ?", id).Scan(&jobData)
	if err != nil {
		if err == sql.ErrNoRows {
			return fmt.Errorf("errored item not found: %s", id)
		}
		return fmt.Errorf("failed to get errored item: %w", err)
	}

	var job FileJob
	if err := json.Unmarshal(jobData, &job); err != nil {
		return fmt.Errorf("failed to unmarshal job data: %w", err)
	}

	// Reset retry count and re-add to queue
	job.RetryCount = 0
	if err := q.ReaddJob(ctx, &job); err != nil {
		return fmt.Errorf("failed to re-add job to queue: %w", err)
	}

	// Delete from errored items
	_, err = q.db.Exec("DELETE FROM errored_items WHERE id = ?", id)
	if err != nil {
		return fmt.Errorf("failed to delete errored item: %w", err)
	}

	return nil
}

// UpdateScriptStatus updates the script execution status for a completed item.
// If firstFailureAt is provided and it's the first failure (retryCount == 1), it will be recorded.
func (q *Queue) UpdateScriptStatus(ctx context.Context, itemID string, status string, retryCount int, lastError string, nextRetryAt *time.Time, firstFailureAt *time.Time) error {
	var nextRetryAtStr sql.NullString
	if nextRetryAt != nil {
		nextRetryAtStr = sql.NullString{String: nextRetryAt.Format("2006-01-02T15:04:05.000Z"), Valid: true}
	}

	var lastErrorStr sql.NullString
	if lastError != "" {
		lastErrorStr = sql.NullString{String: lastError, Valid: true}
	}

	var firstFailureAtStr sql.NullString
	if firstFailureAt != nil {
		firstFailureAtStr = sql.NullString{String: firstFailureAt.Format("2006-01-02T15:04:05.000Z"), Valid: true}
	}

	// Only set first_failure_at if it's provided and the column is currently NULL
	_, err := q.db.ExecContext(ctx, `
		UPDATE completed_items
		SET script_status = ?,
		    script_retry_count = ?,
		    script_last_error = ?,
		    script_next_retry_at = ?,
		    script_first_failure_at = COALESCE(script_first_failure_at, ?)
		WHERE id = ?
	`, status, retryCount, lastErrorStr, nextRetryAtStr, firstFailureAtStr, itemID)

	if err != nil {
		return fmt.Errorf("failed to update script status: %w", err)
	}

	return nil
}

// MarkScriptCompleted marks the script execution as completed for a completed item
func (q *Queue) MarkScriptCompleted(ctx context.Context, itemID string) error {
	_, err := q.db.ExecContext(ctx, `
		UPDATE completed_items
		SET script_status = 'completed',
		    script_last_error = NULL,
		    script_next_retry_at = NULL,
		    script_first_failure_at = NULL
		WHERE id = ?
	`, itemID)

	if err != nil {
		return fmt.Errorf("failed to mark script as completed: %w", err)
	}

	return nil
}

// MarkScriptFailed marks the script execution as permanently failed
func (q *Queue) MarkScriptFailed(ctx context.Context, itemID string, lastError string) error {
	_, err := q.db.ExecContext(ctx, `
		UPDATE completed_items
		SET script_status = 'failed_permanent',
		    script_last_error = ?,
		    script_next_retry_at = NULL
		WHERE id = ?
	`, lastError, itemID)

	if err != nil {
		return fmt.Errorf("failed to mark script as failed: %w", err)
	}

	return nil
}

// GetItemsForScriptRetry retrieves completed items that need script retry
func (q *Queue) GetItemsForScriptRetry(ctx context.Context, limit int) ([]CompletedItem, error) {
	now := time.Now().Format("2006-01-02T15:04:05.000Z")

	rows, err := q.db.QueryContext(ctx, `
		SELECT id, path, size, priority, nzb_path, created_at, completed_at, job_data,
		       script_retry_count, script_last_error, script_next_retry_at, script_first_failure_at
		FROM completed_items
		WHERE script_status = 'pending_retry'
		  AND (script_next_retry_at IS NULL OR script_next_retry_at <= ?)
		ORDER BY script_next_retry_at ASC
		LIMIT ?
	`, now, limit)

	if err != nil {
		return nil, fmt.Errorf("failed to query items for script retry: %w", err)
	}
	defer func() {
		_ = rows.Close()
	}()

	var items []CompletedItem
	for rows.Next() {
		var item CompletedItem
		var createdAtStr, completedAtStr, scriptLastErrorStr, scriptNextRetryAtStr, scriptFirstFailureAtStr sql.NullString

		err := rows.Scan(
			&item.ID, &item.Path, &item.Size, &item.Priority, &item.NzbPath,
			&createdAtStr, &completedAtStr, &item.JobData,
			&item.ScriptRetryCount, &scriptLastErrorStr, &scriptNextRetryAtStr, &scriptFirstFailureAtStr,
		)
		if err != nil {
			continue
		}

		if createdAtStr.Valid {
			if t, err := time.Parse("2006-01-02T15:04:05.000Z", createdAtStr.String); err == nil {
				item.CreatedAt = t
			}
		}
		if completedAtStr.Valid {
			if t, err := time.Parse("2006-01-02T15:04:05.000Z", completedAtStr.String); err == nil {
				item.CompletedAt = t
			}
		}
		if scriptFirstFailureAtStr.Valid {
			if t, err := time.Parse("2006-01-02T15:04:05.000Z", scriptFirstFailureAtStr.String); err == nil {
				item.ScriptFirstFailureAt = &t
			}
		}

		items = append(items, item)
	}

	return items, rows.Err()
}

// PendingArticleCheck represents an article that needs deferred verification
type PendingArticleCheck struct {
	ID              int64
	CompletedItemID string
	MessageID       string
	Groups          string // JSON-encoded array of group names
	Status          string // pending, verified, failed
	RetryCount      int
	NextRetryAt     time.Time
	FirstFailureAt  time.Time
	LastCheckedAt   *time.Time
	CreatedAt       time.Time
}

// AddPendingArticleChecks inserts pending article checks for a completed item
func (q *Queue) AddPendingArticleChecks(ctx context.Context, completedItemID string, articles []PendingArticleCheck) error {
	tx, err := q.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("failed to begin transaction: %w", err)
	}
	defer func() { _ = tx.Rollback() }()

	stmt, err := tx.PrepareContext(ctx, `
		INSERT INTO pending_article_checks (completed_item_id, message_id, groups, status, next_retry_at)
		VALUES (?, ?, ?, 'pending', ?)
	`)
	if err != nil {
		return fmt.Errorf("failed to prepare statement: %w", err)
	}
	defer func() { _ = stmt.Close() }()

	for _, article := range articles {
		nextRetry := article.NextRetryAt.Format("2006-01-02T15:04:05.000Z")
		_, err := stmt.ExecContext(ctx, completedItemID, article.MessageID, article.Groups, nextRetry)
		if err != nil {
			return fmt.Errorf("failed to insert pending article check: %w", err)
		}
	}

	return tx.Commit()
}

// GetArticlesForCheck retrieves pending articles that are ready for checking
func (q *Queue) GetArticlesForCheck(ctx context.Context, limit int) ([]PendingArticleCheck, error) {
	now := time.Now().Format("2006-01-02T15:04:05.000Z")

	rows, err := q.db.QueryContext(ctx, `
		SELECT id, completed_item_id, message_id, groups, status, retry_count,
			   next_retry_at, first_failure_at, last_checked_at, created_at
		FROM pending_article_checks
		WHERE status = 'pending' AND next_retry_at <= ?
		ORDER BY next_retry_at ASC
		LIMIT ?
	`, now, limit)
	if err != nil {
		return nil, fmt.Errorf("failed to query pending article checks: %w", err)
	}
	defer func() { _ = rows.Close() }()

	var items []PendingArticleCheck
	for rows.Next() {
		var item PendingArticleCheck
		var nextRetryStr, firstFailureStr, createdStr string
		var lastCheckedStr sql.NullString

		err := rows.Scan(
			&item.ID, &item.CompletedItemID, &item.MessageID, &item.Groups,
			&item.Status, &item.RetryCount,
			&nextRetryStr, &firstFailureStr, &lastCheckedStr, &createdStr,
		)
		if err != nil {
			continue
		}

		item.NextRetryAt, _ = time.Parse("2006-01-02T15:04:05.000Z", nextRetryStr)
		item.FirstFailureAt, _ = time.Parse("2006-01-02T15:04:05.000Z", firstFailureStr)
		item.CreatedAt, _ = time.Parse("2006-01-02T15:04:05.000Z", createdStr)
		if lastCheckedStr.Valid {
			t, _ := time.Parse("2006-01-02T15:04:05.000Z", lastCheckedStr.String)
			item.LastCheckedAt = &t
		}

		items = append(items, item)
	}

	return items, rows.Err()
}

// MarkArticleVerified marks a pending article check as verified
func (q *Queue) MarkArticleVerified(ctx context.Context, id int64) error {
	now := time.Now().Format("2006-01-02T15:04:05.000Z")
	_, err := q.db.ExecContext(ctx, `
		UPDATE pending_article_checks SET status = 'verified', last_checked_at = ? WHERE id = ?
	`, now, id)
	return err
}

// MarkArticleCheckFailed marks a pending article check as permanently failed
func (q *Queue) MarkArticleCheckFailed(ctx context.Context, id int64) error {
	now := time.Now().Format("2006-01-02T15:04:05.000Z")
	_, err := q.db.ExecContext(ctx, `
		UPDATE pending_article_checks SET status = 'failed', last_checked_at = ? WHERE id = ?
	`, now, id)
	return err
}

// UpdateArticleCheckRetry updates a pending article check for the next retry
func (q *Queue) UpdateArticleCheckRetry(ctx context.Context, id int64, retryCount int, nextRetryAt time.Time) error {
	now := time.Now().Format("2006-01-02T15:04:05.000Z")
	nextRetry := nextRetryAt.Format("2006-01-02T15:04:05.000Z")
	_, err := q.db.ExecContext(ctx, `
		UPDATE pending_article_checks SET retry_count = ?, next_retry_at = ?, last_checked_at = ? WHERE id = ?
	`, retryCount, nextRetry, now, id)
	return err
}

// UpdateCompletedItemVerificationStatus updates the verification status of a completed item
func (q *Queue) UpdateCompletedItemVerificationStatus(ctx context.Context, completedItemID string, status string) error {
	_, err := q.db.ExecContext(ctx, `
		UPDATE completed_items SET verification_status = ? WHERE id = ?
	`, status, completedItemID)
	return err
}

// GetPendingCheckCountForItem returns the total, pending, and failed count of article checks for a completed item
func (q *Queue) GetPendingCheckCountForItem(ctx context.Context, completedItemID string) (total int, pending int, failed int, err error) {
	err = q.db.QueryRowContext(ctx, `
		SELECT
			COUNT(*) as total,
			SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END) as pending,
			SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) as failed
		FROM pending_article_checks
		WHERE completed_item_id = ?
	`, completedItemID).Scan(&total, &pending, &failed)
	return
}
