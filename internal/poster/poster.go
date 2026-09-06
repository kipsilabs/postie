package poster

import (
	"context"
	"crypto/md5"
	"errors"
	"fmt"
	"log/slog"
	"math/rand"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/google/uuid"
	"github.com/javi11/nxg"
	"github.com/kipsilabs/postie/internal/article"
	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/manifest"
	"github.com/kipsilabs/postie/internal/nzb"
	"github.com/kipsilabs/postie/internal/par2"
	"github.com/kipsilabs/postie/internal/pausable"
	"github.com/kipsilabs/postie/internal/pool"
	"github.com/kipsilabs/postie/internal/progress"
	concpool "github.com/sourcegraph/conc/pool"
)

var (
	// ErrPosterClosed is returned when attempting to post after the poster has been closed
	ErrPosterClosed = errors.New("poster is closed")
)

// defaultBodyBufferSize is the baseline allocation for the body buffer pool
// (slightly larger than the default article size of 750KB).
const defaultBodyBufferSize = 768 * 1024

// readAtStallTimeout bounds a single ReadAt call. *os.File.ReadAt is not
// cancellable via context (Go limitation on regular files), so when the
// underlying mount stalls — NFS, sleeping external drive, FUSE backend —
// the read-ahead goroutine would otherwise hang forever, holding a buffer
// and blocking the upload pipeline. We surface a clean error after this
// deadline so the post fails instead of deadlocking. The OS-level read
// goroutine itself still leaks until the kernel returns, but the upload
// pipeline keeps moving and operators get a clear log line.
const readAtStallTimeout = 60 * time.Second

// maxBodyBufferPoolSize is the upper bound on buffers we accept back into the
// pool. Anything larger came from an anomalously large article and would
// permanently inflate the pool's working set if reused, so we drop it.
const maxBodyBufferPoolSize = 4 * defaultBodyBufferSize

// bodyBufferPool provides reusable buffers for article body read-ahead to reduce GC pressure
var bodyBufferPool = sync.Pool{
	New: func() any {
		return make([]byte, defaultBodyBufferSize)
	},
}

// readAtWithStallGuard performs file.ReadAt under a wall-clock deadline. If
// ReadAt does not return within readAtStallTimeout (e.g. the backing mount is
// unresponsive), it returns a stall error so the caller can fail the post
// cleanly instead of blocking the upload pipeline forever. The inner
// goroutine still blocks until the OS releases ReadAt; this is a fundamental
// Go limitation for regular files.
func readAtWithStallGuard(ctx context.Context, file *os.File, buf []byte, off int64) (int, error) {
	type result struct {
		n   int
		err error
	}
	resCh := make(chan result, 1)
	go func() {
		n, err := file.ReadAt(buf, off)
		resCh <- result{n: n, err: err}
	}()
	select {
	case r := <-resCh:
		return r.n, r.err
	case <-ctx.Done():
		return 0, ctx.Err()
	case <-time.After(readAtStallTimeout):
		return 0, fmt.Errorf("read stalled after %s at offset %d (filesystem unresponsive?)", readAtStallTimeout, off)
	}
}

// putBodyBuffer returns a buffer to the pool, dropping it if it grew beyond
// maxBodyBufferPoolSize so one outlier article cannot inflate the pool forever.
func putBodyBuffer(buf []byte) {
	if buf == nil {
		return
	}
	full := buf[:cap(buf)]
	if cap(full) > maxBodyBufferPoolSize {
		return
	}
	bodyBufferPool.Put(full) //nolint:staticcheck // SA6002: slices have pointer semantics, no wrapper needed
}

// ManifestSink records a durable manifest for a file's articles before they are
// posted. It is implemented outside the poster (by the transfer runtime) and
// injected per job; a nil sink disables manifest recording (standalone mode).
type ManifestSink interface {
	RecordFile(ctx context.Context, filePath string, articles []*article.Article) error
	// ExistingArticles returns a previously recorded manifest's article records
	// for filePath (ok=false if none), used for crash recovery to reuse the
	// exact Message-IDs rather than regenerating them.
	ExistingArticles(ctx context.Context, filePath string) ([]manifest.ArticleRecord, bool, error)
}

// Poster defines the interface for posting articles to Usenet
type Poster interface {
	// Post posts files from a directory to Usenet
	Post(ctx context.Context, files []string, rootDir string, nzbGen nzb.NZBGenerator) error
	// PostWithRelativePaths posts files with custom display names (relative paths) for subjects
	// relativePaths maps absolute file path to the display name to use in the subject
	PostWithRelativePaths(ctx context.Context, files []string, rootDir string, nzbGen nzb.NZBGenerator, relativePaths map[string]string) error
	// Stats returns a point-in-time snapshot of posting statistics
	Stats() StatsSnapshot
	// Close closes the poster
	Close()
}

// PostStatus represents the status of a post
type PostStatus int

const (
	PostStatusPending PostStatus = iota
	PostStatusPosted
	PostStatusVerified
	PostStatusFailed
	PostStatusCancelled
	PostStatusPosting
)

// Post represents a file to be posted
type Post struct {
	FilePath string
	Articles []*article.Article
	Status   PostStatus
	Error    error
	Retries  int
	file     *os.File
	mu       sync.Mutex
	filesize int64
	wg       *sync.WaitGroup
	failed   *atomic.Int64
	progress progress.Progress
}

// FailedArticleInfo contains information about an article that failed verification
// and should be deferred for later checking.
type FailedArticleInfo struct {
	MessageID string
	Groups    []string
}

// DeferredCheckError is a non-fatal error indicating some articles need deferred verification.
// The upload itself succeeded, but article verification (STAT check) failed after all immediate retries.
type DeferredCheckError struct {
	FailedArticles []FailedArticleInfo
	TotalArticles  int
}

func (e *DeferredCheckError) Error() string {
	return fmt.Sprintf("%d/%d articles deferred for later verification", len(e.FailedArticles), e.TotalArticles)
}

// articleWithBody holds an article with its pre-read body data for read-ahead buffering
type articleWithBody struct {
	article  *article.Article
	body     []byte
	poolBuf  []byte // original pooled buffer (may be larger than body)
	reserved int64  // bytes reserved from the engine buffer budget (0 if no engine)
}

// uploadJob is submitted to the shared poster worker pool. Each job posts one
// article. The done callback is invoked exactly once, on success or failure,
// so the submitter can track per-Post completion and capture the first error.
// The worker is responsible for returning poolBuf to the body buffer pool.
type uploadJob struct {
	ctx      context.Context
	art      *article.Article
	body     []byte
	poolBuf  []byte
	reserved int64 // engine buffer-budget bytes to release when the job ends
	done     func(err error)
}

// Stats is the mutable, mutex-guarded holder for posting statistics. Consumers
// read point-in-time copies via Stats()/Reposter.Stats(), which return a
// StatsSnapshot (no lock) so the mutex is never copied.
type Stats struct {
	ArticlesPosted  int64
	ArticlesChecked int64
	BytesPosted     int64
	ArticleErrors   int64
	StartTime       time.Time
	mu              sync.Mutex
}

// StatsSnapshot is a point-in-time copy of posting statistics, safe to copy
// and pass by value.
type StatsSnapshot struct {
	ArticlesPosted  int64
	ArticlesChecked int64
	BytesPosted     int64
	ArticleErrors   int64
	StartTime       time.Time
}

// snapshot returns a lock-free copy of the current counters.
func (s *Stats) snapshot() StatsSnapshot {
	s.mu.Lock()
	defer s.mu.Unlock()
	return StatsSnapshot{
		ArticlesPosted:  s.ArticlesPosted,
		ArticlesChecked: s.ArticlesChecked,
		BytesPosted:     s.BytesPosted,
		ArticleErrors:   s.ArticleErrors,
		StartTime:       s.StartTime,
	}
}

// Poster handles posting articles to Usenet
type poster struct {
	cfg         config.PostingConfig
	checkCfg    config.PostCheck
	uploadPool  pool.NNTPClient // Pool for posting articles
	verifyPool  pool.NNTPClient // Pool for article verification (may be same as uploadPool)
	stats       *Stats
	throttle    *Throttle
	jobProgress progress.JobProgress
	closed      atomic.Bool
	closeOnce   sync.Once

	// Shared upload worker pool. Sized once to the total number of upload-pool
	// connections so concurrent Post() calls share workers instead of each
	// spawning its own pool. See issue #184. Initialized lazily via workerInit
	// so that tests which build the struct directly still get a working pool
	// on the first Post() call.
	workerInit       sync.Once
	numOfConnections int
	uploadJobs       chan uploadJob
	shutdown         chan struct{}
	workerWG         sync.WaitGroup

	// engine, when non-nil, bounds effective upload concurrency and in-flight
	// buffer memory across ALL jobs process-wide. The per-poster worker pool
	// above still pumps articles, but each post acquires a global engine worker
	// slot and each read-ahead buffer is reserved against the global byte
	// budget, so concurrent jobs no longer multiply workers or memory. Nil =
	// standalone behaviour (no global gate).
	engine *Engine

	// manifestSink, when non-nil, records a durable manifest for each file
	// before its articles are posted. Nil = standalone (no manifest recording).
	manifestSink ManifestSink
}

// ensureWorkersStarted spins up the shared upload worker pool exactly once.
// Safe to call from both New() (eager) and Post() (lazy fallback for tests).
func (p *poster) ensureWorkersStarted() {
	p.workerInit.Do(func() {
		if p.uploadJobs == nil {
			p.uploadJobs = make(chan uploadJob)
		}
		if p.shutdown == nil {
			p.shutdown = make(chan struct{})
		}
		if p.numOfConnections <= 0 {
			n := 0
			if p.uploadPool != nil {
				for _, pr := range p.uploadPool.Stats().Providers {
					n += pr.MaxConnections
				}
			}
			if n < 1 {
				n = 1
			}
			p.numOfConnections = n
		}
		for i := 0; i < p.numOfConnections; i++ {
			p.workerWG.Add(1)
			go p.uploadWorker()
		}
	})
}

// New creates a new poster using dependency injection for the connection pool manager
func New(ctx context.Context, cfg config.Config, poolManager pool.PoolManager, jobProgress progress.JobProgress) (Poster, error) {
	return NewWithEngine(ctx, cfg, poolManager, jobProgress, nil, nil)
}

// NewWithEngine creates a poster that routes upload work through the supplied
// process-wide Engine, so effective concurrency and in-flight buffer memory are
// bounded across all jobs. A nil engine preserves standalone behaviour. The
// optional manifestSink records a durable manifest per file before posting; a
// nil sink disables manifest recording.
func NewWithEngine(ctx context.Context, cfg config.Config, poolManager pool.PoolManager, jobProgress progress.JobProgress, engine *Engine, manifestSink ManifestSink) (Poster, error) {
	if poolManager == nil {
		return nil, fmt.Errorf("pool manager cannot be nil")
	}

	// Get the upload pool from the manager
	uploadPool := poolManager.GetUploadPool()
	if uploadPool == nil {
		return nil, fmt.Errorf("connection pool is not available")
	}

	// Get the verify pool (may be same as upload pool if no verify-role servers configured)
	verifyPool := poolManager.GetVerifyPool()
	if verifyPool == nil {
		verifyPool = uploadPool
	}

	stats := &Stats{
		StartTime: time.Now(),
	}

	postCheckCfg := cfg.GetPostCheckConfig()

	p := &poster{
		cfg:          cfg.GetPostingConfig(),
		checkCfg:     postCheckCfg,
		uploadPool:   uploadPool,
		verifyPool:   verifyPool,
		stats:        stats,
		jobProgress:  jobProgress,
		engine:       engine,
		manifestSink: manifestSink,
	}

	// Size the shared upload worker pool to the total number of upload-pool
	// connections across all providers. This caps in-process posting goroutines
	// to actual upstream capacity rather than letting each Post() call spawn
	// its own pool, which previously multiplied with MaxConcurrentUploads × PAR2.
	p.ensureWorkersStarted()

	// Log post check configuration for debugging
	if postCheckCfg.Enabled != nil {
		slog.DebugContext(ctx, "Poster initialized",
			"post_check_enabled", *postCheckCfg.Enabled,
			"max_repost", postCheckCfg.MaxRePost,
			"retry_delay", postCheckCfg.RetryDelay)
	} else {
		slog.WarnContext(ctx, "PostCheck.Enabled is nil, will default to true")
	}

	// Use the engine-wide throttle so the configured rate bounds aggregate
	// egress across all jobs and the durable re-poster (nil engine falls back
	// to a private bucket for standalone mode).
	p.throttle = p.engine.SharedThrottle(p.cfg.ThrottleRate)

	return p, nil
}

// immediateCheckEnabled reports whether the legacy in-poster checkLoop runs.
// It is disabled in durable mode (a manifest sink is set): verification then
// happens in the background durable verification service, which lets the upload
// slot be released before propagation delay.
func (p *poster) immediateCheckEnabled() bool {
	return p.manifestSink == nil && p.checkCfg.Enabled != nil && *p.checkCfg.Enabled
}

func (p *poster) Close() {
	p.closeOnce.Do(func() {
		p.closed.Store(true)
		// Only signal workers if they were ever started. A Post() call would
		// have triggered ensureWorkersStarted; otherwise shutdown is nil.
		if p.shutdown != nil {
			close(p.shutdown)
			p.workerWG.Wait()
		}
		slog.Info("Poster closed - no new Post() calls will be accepted")
	})
}

// uploadWorker drains uploadJob entries from the shared queue and posts each
// article. Exactly numOfConnections workers run for the lifetime of the
// poster, so total posting concurrency is bounded regardless of how many
// concurrent Post() calls are in flight.
func (p *poster) uploadWorker() {
	defer p.workerWG.Done()
	for {
		select {
		case <-p.shutdown:
			return
		case job := <-p.uploadJobs:
			p.runUploadJob(job)
		}
	}
}

// runUploadJob executes a single upload job and reports the outcome through
// job.done. The job's body buffer is always returned to the pool, even on
// error or cancellation.
func (p *poster) runUploadJob(job uploadJob) {
	defer func() {
		putBodyBuffer(job.poolBuf)
		// Release the global buffer-budget reservation for this article.
		p.engine.ReleaseBuffer(job.reserved)
	}()

	if err := job.ctx.Err(); err != nil {
		job.done(err)
		return
	}
	if err := pausable.CheckPause(job.ctx); err != nil {
		job.done(err)
		return
	}
	// Acquire a process-wide worker slot so effective posting concurrency across
	// all jobs stays within the engine's worker_count (nil engine = no gate).
	if err := p.engine.AcquireWorker(job.ctx); err != nil {
		job.done(err)
		return
	}
	defer p.engine.ReleaseWorker()

	job.done(p.postArticleWithBody(job.ctx, job.art, job.body))
}

// Post posts files from a directory to Usenet
func (p *poster) Post(
	ctx context.Context,
	files []string,
	rootDir string,
	nzbGen nzb.NZBGenerator,
) error {
	return p.PostWithRelativePaths(ctx, files, rootDir, nzbGen, nil)
}

// PostWithRelativePaths posts files with custom display names (relative paths) for subjects
// relativePaths maps absolute file path to the display name to use in the subject
// If relativePaths is nil or a file is not found in the map, the filename is used
func (p *poster) PostWithRelativePaths(
	ctx context.Context,
	files []string,
	rootDir string,
	nzbGen nzb.NZBGenerator,
	relativePaths map[string]string,
) error {
	// Check if poster has been closed
	if p.closed.Load() {
		return ErrPosterClosed
	}

	// Lazy fallback for struct-literal construction (tests). New() already
	// invokes this; second call is a no-op via sync.Once.
	p.ensureWorkersStarted()

	wg := sync.WaitGroup{}
	var failedPosts atomic.Int64

	// Create a context that can be canceled
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()

	// Create error channel to collect errors. Buffered enough that the deferred
	// writers (postLoop, checkLoop, deferred-check sender) cannot block each
	// other if more than one error fires before the main goroutine drains.
	errChan := make(chan error, 4)

	// Create channels for post and check queues
	postQueue := make(chan *Post, 100)
	checkQueue := make(chan *Post, 100)

	// Track posts in flight (initial + retries) so we close postQueue only once
	// every initial post AND every retry is fully accounted for. Closing earlier
	// races with checkLoop's retry sends and panics on closed-channel send.
	var postsInFlight sync.WaitGroup

	// Start a goroutine to process posts
	go p.postLoop(ctx, postQueue, checkQueue, errChan, nzbGen, &postsInFlight)

	// Start a goroutine to process checks only if the in-poster check is enabled.
	// In durable mode (manifest sink set) verification is handled by the
	// background durable verification service instead, so the legacy checkLoop is
	// skipped and the upload slot is released as soon as posting finishes.
	if p.immediateCheckEnabled() {
		go p.checkLoop(ctx, checkQueue, postQueue, errChan, nzbGen, &postsInFlight)
		slog.DebugContext(ctx, "Post check enabled - started checkLoop goroutine")
	} else {
		slog.InfoContext(ctx, "In-poster check disabled - verification deferred to durable service or skipped")
	}

	wg.Add(len(files))
	for i, file := range files {
		// Check if context is canceled before adding more posts
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}

		// Get the display name (relative path) for this file, or empty string to use filename
		displayName := ""
		if relativePaths != nil {
			displayName = relativePaths[file]
		}

		if err := p.addPost(ctx, file, displayName, i+1, len(files), &wg, &failedPosts, postQueue, nzbGen, &postsInFlight); err != nil {
			return fmt.Errorf("error adding file %s to posting queue: %w", file, err)
		}
	}

	// Close postQueue only when no posts are in-flight (initial + any retries
	// queued by checkLoop). This avoids the closed-channel panic on retry sends
	// and lets postLoop/checkLoop drain naturally.
	go func() {
		postsInFlight.Wait()
		close(postQueue)
	}()

	// Wait for all posts to complete or an error to occur
	done := make(chan struct{})
	go func() {
		wg.Wait()
		close(done)
	}()

	// Collect any deferred check error that arrives after posting completes
	var deferredErr *DeferredCheckError

	select {
	case <-ctx.Done():
		cancel() // Cancel the context to stop all operations
		return ctx.Err()
	case err := <-errChan:
		// Check if this is a non-fatal DeferredCheckError
		if errors.As(err, &deferredErr) {
			// Don't cancel - this is non-fatal, wait for completion
		} else {
			cancel() // Cancel the context to stop all operations
			return err
		}
	case <-done:
		// All posts completed normally
	}

	// If we got a deferred error, wait for done signal too
	if deferredErr != nil {
		select {
		case <-done:
		case <-ctx.Done():
			return ctx.Err()
		}
	}

	// Check for any additional error that may have arrived
	select {
	case err := <-errChan:
		if !errors.As(err, &deferredErr) {
			return err
		}
	default:
	}

	if n := failedPosts.Load(); n > 0 {
		return fmt.Errorf("failed to post %d files", n)
	}

	// Return deferred error if present (non-fatal - caller should handle)
	if deferredErr != nil {
		return deferredErr
	}

	return nil
}

// abandonPost releases everything held by a post that will never be processed
// because its loop exited early: finishes its progress entry, closes the file,
// and balances the in-flight queue counter and the per-file WaitGroup so
// Post()'s completion goroutines can terminate instead of leaking.
func (p *poster) abandonPost(ctx context.Context, post *Post, postsInFlight *sync.WaitGroup) {
	post.mu.Lock()
	if post.Status != PostStatusFailed {
		post.Status = PostStatusCancelled
	}
	post.mu.Unlock()

	if post.progress != nil {
		p.jobProgress.FinishProgress(post.progress.GetID())
	}
	if post.file != nil {
		if err := post.file.Close(); err != nil {
			slog.WarnContext(ctx, "Error closing file for abandoned post", "error", err, "file", post.FilePath)
		}
	}
	postsInFlight.Done()
	post.wg.Done()
}

// drainAbandoned consumes every post left on q after its consumer loop exits
// early and abandons each one, so queued posts don't leak file descriptors and
// WaitGroup counts. It runs in its own goroutine because q is only closed once
// postsInFlight reaches zero — which the draining itself brings about. On a
// normal loop exit q is already closed and empty, so the goroutine ends
// immediately.
func (p *poster) drainAbandoned(ctx context.Context, q <-chan *Post, postsInFlight *sync.WaitGroup) {
	go func() {
		for post := range q {
			p.abandonPost(ctx, post, postsInFlight)
		}
	}()
}

// postLoop processes posts from the queue
// postPipelineDepth is how many posts (files) a single Post() call processes
// concurrently. Overlapping the tail of one file's upload with the next file's
// read-ahead keeps the shared worker pool busy across file boundaries (with
// depth 1 the pool drained toward idle at every boundary on many-small-file
// jobs). Total article concurrency is still bounded by the worker pool and the
// engine's buffer budget, so this does not increase peak memory.
const postPipelineDepth = 2

func (p *poster) postLoop(ctx context.Context, postQueue chan *Post, checkQueue chan *Post, errChan chan<- error, nzbGen nzb.NZBGenerator, postsInFlight *sync.WaitGroup) {
	sem := make(chan struct{}, postPipelineDepth)
	var inFlight sync.WaitGroup

	// checkQueue is written to by the processPost goroutines, so it can only be
	// closed after every one of them has finished.
	defer func() {
		inFlight.Wait()
		close(checkQueue)
	}()
	// On any exit, abandon posts still queued so their files/WaitGroup counts
	// don't leak when this loop stops early on a fatal error.
	defer p.drainAbandoned(ctx, postQueue, postsInFlight)

	// The first fatal error stops intake (remaining posts are abandoned by the
	// drain) and is reported to errChan at most once, matching the previous
	// serial return-on-first-error semantics.
	var stopped atomic.Bool
	var reportOnce sync.Once
	reportFatal := func(err error) {
		stopped.Store(true)
		if err != nil {
			reportOnce.Do(func() { errChan <- err })
		}
	}

	for post := range postQueue {
		if err := ctx.Err(); err != nil {
			reportFatal(err)
		}
		if stopped.Load() {
			p.abandonPost(ctx, post, postsInFlight)
			continue
		}
		// Check if we should pause before processing the post
		if err := pausable.CheckPause(ctx); err != nil {
			reportFatal(err)
			p.abandonPost(ctx, post, postsInFlight)
			continue
		}

		sem <- struct{}{}
		inFlight.Add(1)
		go func(post *Post) {
			defer func() {
				<-sem
				inFlight.Done()
			}()
			p.processPost(ctx, post, checkQueue, nzbGen, postsInFlight, reportFatal)
		}(post)
	}
}

// processPost posts one file's articles through the shared upload worker pool,
// then hands the post to the check queue (immediate-check mode) or completes
// it. It always balances the post's accounting (postsInFlight, per-file wg,
// file handle) and reports fatal errors through reportFatal.
func (p *poster) processPost(ctx context.Context, post *Post, checkQueue chan<- *Post, nzbGen nzb.NZBGenerator, postsInFlight *sync.WaitGroup, reportFatal func(error)) {
	// Set post status to Posting
	post.mu.Lock()
	post.Status = PostStatusPosting
	post.mu.Unlock()
	{

		// Per-post context so the read-ahead goroutine always terminates
		// when this post's block exits, even if the parent ctx is still
		// alive (e.g. on the deferred-check non-fatal-error path).
		postCtx, postCancel := context.WithCancel(ctx)

		// Create read-ahead channel (buffer 50 articles ahead to overlap I/O with network)
		readAheadChan := make(chan articleWithBody, 50)

		// Per-article reservation against the process-wide buffer budget; 0
		// when no engine is configured (reservation is then a no-op).
		reserveBytes := p.engine.PerArticleBytes()

		var articleWG sync.WaitGroup
		var firstErr atomic.Pointer[error]
		recordErr := func(err error) {
			if err == nil {
				return
			}
			e := err
			firstErr.CompareAndSwap(nil, &e)
		}

		// Start read-ahead goroutine to pre-read article bodies
		go func() {
			defer close(readAheadChan)
			for _, art := range post.Articles {
				select {
				case <-postCtx.Done():
					return
				default:
					// Reserve global buffer budget before allocating/reading.
					// This blocks (and so bounds total in-flight memory across
					// all jobs) when the budget is exhausted.
					if err := p.engine.ReserveBuffer(postCtx, reserveBytes); err != nil {
						// Usually postCtx cancellation, but record it so a
						// non-cancel failure can't silently truncate the post.
						recordErr(fmt.Errorf("error reserving buffer for %s: %w", post.FilePath, err))
						return
					}

					// Get buffer from pool, resize if needed. Outliers are dropped
					// on Put (see putBodyBuffer) so they cannot inflate the pool.
					poolBuf := bodyBufferPool.Get().([]byte)
					if cap(poolBuf) < int(art.Size) {
						poolBuf = make([]byte, art.Size)
					}
					body := poolBuf[:art.Size]

					if _, err := readAtWithStallGuard(postCtx, post.file, body, art.Offset); err != nil {
						putBodyBuffer(poolBuf)
						p.engine.ReleaseBuffer(reserveBytes)
						slog.ErrorContext(ctx, "Error pre-reading article", "error", err, "offset", art.Offset)
						// Propagate the read failure: without this the
						// remaining segments are silently dropped and the
						// post is reported as fully posted with a
						// truncated article set / NZB.
						recordErr(fmt.Errorf("error pre-reading article at offset %d of %s: %w", art.Offset, post.FilePath, err))
						return
					}

					select {
					case readAheadChan <- articleWithBody{article: art, body: body, poolBuf: poolBuf, reserved: reserveBytes}:
					case <-postCtx.Done():
						putBodyBuffer(poolBuf)
						p.engine.ReleaseBuffer(reserveBytes)
						return
					}
				}
			}
		}()

		// Submit articles to the shared upload worker pool. Total posting
		// concurrency across all in-flight Post() calls is capped at
		// p.numOfConnections; this prevents the goroutine + buffer explosion
		// that caused OOM under MaxConcurrentUploads > 1 with PAR2 enabled.
		//
		// We intentionally do NOT cancel sibling articles when one fails
		// (e.g. TLS timeout) — they should continue.

		// Collect completed articles for batch NZB addition (reduces lock contention)
		var completedArticles []*article.Article
		var completedMu sync.Mutex

		for artWithBody := range readAheadChan {
			art := artWithBody.article
			body := artWithBody.body
			poolBuf := artWithBody.poolBuf
			reserved := artWithBody.reserved

			articleWG.Add(1)
			job := uploadJob{
				ctx:      postCtx,
				art:      art,
				body:     body,
				poolBuf:  poolBuf,
				reserved: reserved,
				done: func(err error) {
					defer articleWG.Done()
					if err != nil {
						recordErr(err)
						return
					}
					post.progress.UpdateProgress(int64(art.Size))
					completedMu.Lock()
					completedArticles = append(completedArticles, art)
					completedMu.Unlock()
				},
			}

			select {
			case p.uploadJobs <- job:
			case <-p.shutdown:
				putBodyBuffer(poolBuf)
				p.engine.ReleaseBuffer(reserved)
				recordErr(ErrPosterClosed)
				articleWG.Done()
			case <-postCtx.Done():
				putBodyBuffer(poolBuf)
				p.engine.ReleaseBuffer(reserved)
				recordErr(postCtx.Err())
				articleWG.Done()
			}
		}

		// Wait for all submitted articles to finish.
		articleWG.Wait()

		// Collect first error (if any). Matches the previous WithFirstError
		// semantic from conc/pool: other workers continued; we surface the
		// first failure to the caller.
		var errs error
		if ePtr := firstErr.Load(); ePtr != nil {
			errs = *ePtr
		}

		// Read-ahead goroutine has finished (readAheadChan was drained above)
		// but cancel the per-post ctx so any straggler observes Done.
		postCancel()

		// Batch add completed articles to NZB generator (reduces lock contention)
		for _, art := range completedArticles {
			nzbGen.AddArticle(art)
		}

		p.jobProgress.FinishProgress(post.progress.GetID())

		if errs != nil {
			post.mu.Lock()
			if errors.Is(errs, context.Canceled) {
				post.Status = PostStatusCancelled
				post.Error = fmt.Errorf("posting cancelled: %v", errs)
			} else {
				post.Status = PostStatusFailed
				post.Error = fmt.Errorf("failed to post articles: %v", errs)
			}
			post.mu.Unlock()

			// Mark this post as done in the queue tracking
			postsInFlight.Done()

			// Close the underlying file so the descriptor isn't leaked on
			// failure. Long-running daemons that hit intermittent NNTP
			// errors otherwise exhaust the process fd ulimit and stall.
			if post.file != nil {
				if cerr := post.file.Close(); cerr != nil {
					slog.WarnContext(ctx, "Error closing file handle on post failure", "error", cerr, "file", post.FilePath)
				}
			}

			// Stop intake of further posts. A cancellation is not reported
			// (Post() observes ctx.Done itself); a real failure is sent to
			// errChan (at most once) BEFORE wg.Done so Post() cannot wake
			// on `done` before the error is observable.
			if errors.Is(errs, context.Canceled) {
				reportFatal(nil)
			} else {
				reportFatal(fmt.Errorf("failed to post file %s after %d retries: %v", post.FilePath, p.cfg.MaxRetries, errs))
			}

			// Balance the per-file WaitGroup so Post()'s wg.Wait() can
			// complete even when the parent ctx is still alive (e.g. an
			// internal post timeout); otherwise Post() blocks forever.
			post.wg.Done()

			return
		}

		post.mu.Lock()
		post.Status = PostStatusPosted
		post.mu.Unlock()

		if p.immediateCheckEnabled() {
			// Guard the send: if checkLoop has already exited (e.g. on a
			// verify error) the buffered checkQueue can fill and this send
			// would block forever, leaking postLoop and every queued post.
			select {
			case checkQueue <- post:
			case <-ctx.Done():
				if post.file != nil {
					if cerr := post.file.Close(); cerr != nil {
						slog.WarnContext(ctx, "Error closing file after ctx canceled during check enqueue", "error", cerr, "file", post.FilePath)
					}
				}
				postsInFlight.Done()
				post.wg.Done()
			}

			return
		}

		// Post complete without check - mark as done in queue tracking
		postsInFlight.Done()

		// Close file
		if post.file != nil {
			if err := post.file.Close(); err != nil {
				slog.WarnContext(ctx, "Error closing file handle", "error", err, "file", post.FilePath)
			}
		}

		post.wg.Done()
	}
}

// checkLoop processes posts from the check queue
func (p *poster) checkLoop(ctx context.Context, checkQueue chan *Post, postQueue chan *Post, errChan chan<- error, nzbGen nzb.NZBGenerator, postsInFlight *sync.WaitGroup) {
	numOfConnections := 0

	// Use verify pool's providers for connection count
	for _, pr := range p.verifyPool.Stats().Providers {
		numOfConnections += pr.MaxConnections
	}

	// Collect articles that exhaust immediate retries for deferred checking
	var allDeferredArticles []FailedArticleInfo
	var deferredMu sync.Mutex
	totalArticlesProcessed := 0

	deferredEnabled := p.checkCfg.DeferredCheckDelay.ToDuration() > 0

	firstPost := true

	// On any exit, abandon posts still queued so their files/WaitGroup counts
	// don't leak when this loop returns early on a fatal error.
	defer p.drainAbandoned(ctx, checkQueue, postsInFlight)

	for post := range checkQueue {
		select {
		case <-ctx.Done():
			p.abandonPost(ctx, post, postsInFlight)
			errChan <- ctx.Err()
			return
		default:
			// Check if we should pause before processing the check
			if err := pausable.CheckPause(ctx); err != nil {
				p.abandonPost(ctx, post, postsInFlight)
				errChan <- err
				return
			}

			// Create the progress task immediately so the user sees "checking" status
			// during the RetryDelay wait, preventing the queue item from appearing stuck.
			post.progress = p.jobProgress.AddProgress(uuid.New(), fmt.Sprintf("%s (check)", filepath.Base(post.FilePath)), progress.ProgressTypeChecking, post.filesize)

			// Wait for articles to propagate to the verify server before checking.
			// Only needed for the first file: subsequent files are posted after enough
			// time has already elapsed during the first file's posting and checking.
			if firstPost {
				firstPost = false
				if delay := p.checkCfg.RetryDelay.ToDuration(); delay > 0 {
					post.progress.SetWaitDeadline(time.Now().Add(delay))
					select {
					case <-ctx.Done():
						post.progress.SetWaitDeadline(time.Time{})
						p.abandonPost(ctx, post, postsInFlight)
						errChan <- ctx.Err()
						return
					case <-time.After(delay):
					}
					post.progress.SetWaitDeadline(time.Time{})
				}
			}

			// Create a pool with error handling - use all available CPU cores
			// Note: We intentionally don't use WithCancelOnError() to prevent cascading failures
			// when a single article check fails. Other checks should continue.
			pool := concpool.New().WithContext(ctx).WithMaxGoroutines(numOfConnections).WithFirstError()
			articlesChecked := 0
			articleErrors := 0
			var failedArticles []*article.Article
			var mu sync.Mutex

			totalArticlesProcessed += len(post.Articles)

			// Submit all articles to the pool
			for _, art := range post.Articles {
				pool.Go(func(ctx context.Context) error {
					if ctx.Err() != nil {
						return ctx.Err()
					}

					// Check for pause before checking article
					if err := pausable.CheckPause(ctx); err != nil {
						return err
					}

					if err := p.checkArticle(ctx, art); err != nil {
						// Track failed article
						mu.Lock()
						failedArticles = append(failedArticles, art)
						articleErrors++
						mu.Unlock()
						return err
					}

					// Update progress atomically (non-blocking)
					mu.Lock()
					articlesChecked++
					mu.Unlock()

					// Update progress if manager is available
					post.progress.UpdateProgress(int64(art.Size))
					return nil
				})
			}

			// Wait for all workers to complete and collect errors
			errors := pool.Wait()

			// If we have failed articles, handle them
			if len(failedArticles) > 0 {
				post.mu.Lock()
				post.Retries++
				post.mu.Unlock()

				// If we haven't exceeded max retries, add only failed articles back to queue
				if post.Retries < int(p.checkCfg.MaxRePost) {
					// Refresh article headers before re-posting.
					// The MessageID is intentionally preserved: the NZB generator keys entries
					// by (filename, messageID), so regenerating the ID would append a duplicate
					// segment rather than replacing the existing one, corrupting the NZB.
					// Re-using the same MessageID is safe because:
					//   - If the first post was never accepted, the server will accept it again.
					//   - If it was accepted but propagation is slow, the deferred-check path
					//     handles that case and we should not be re-posting at all.
					// The header validation below guards against 441 "Missing required fields"
					// errors that can occur when other headers are missing or empty.
					for _, art := range failedArticles {
						// Ensure required NNTP headers are non-empty
						if art.From == "" {
							if defaultFrom := p.cfg.PostHeaders.DefaultFrom; defaultFrom != "" {
								art.From = defaultFrom
							} else if from, err := article.GenerateFrom(); err == nil {
								art.From = from
							}
						}
						if art.Subject == "" {
							art.Subject = article.GenerateRandomSubject()
						}
						if len(art.Groups) == 0 {
							slog.WarnContext(ctx, "Retried article has no newsgroups", "messageID", art.MessageID)
						}
					}

					// Create a new post with only the failed articles
					failedPost := &Post{
						FilePath: post.FilePath,
						Articles: failedArticles,
						Status:   PostStatusPending,
						file:     post.file,
						filesize: post.filesize,
						wg:       post.wg,
						failed:   post.failed,
						Retries:  post.Retries,
						progress: p.jobProgress.AddProgress(uuid.New(), fmt.Sprintf("%s (retry)", filepath.Base(post.FilePath)), progress.ProgressTypeUploading, post.filesize),
					}

					slog.InfoContext(ctx,
						"Retrying failed articles",
						"file", post.FilePath,
						"attempt", post.Retries,
						"max_retries", p.checkCfg.MaxRePost,
					)

					// Track this retry in the queue before sending
					postsInFlight.Add(1)

					// Send retry. postQueue is kept open by the postsInFlight
					// gate (the Add above runs before this send), so a closed-
					// channel panic here is no longer reachable. Only ctx
					// cancellation can interrupt the send.
					//
					// Bookkeeping: addPost added +1 for the original post; the
					// Add above added +1 for the retry. Once the retry is
					// queued, the original post is no longer "in flight" — the
					// retry represents it from here on — so Done the original.
					select {
					case postQueue <- failedPost:
						postsInFlight.Done()
						continue
					case <-ctx.Done():
						// Decrement the retry that won't be sent, finish its
						// progress entry, then abandon the original post
						// (closes the file and balances its counters).
						postsInFlight.Done()
						p.jobProgress.FinishProgress(failedPost.progress.GetID())
						p.abandonPost(ctx, post, postsInFlight)
						slog.WarnContext(ctx, "Context canceled while trying to send retry", "file", post.FilePath)
						return
					}
				}

				// Max retries exhausted - check if deferred checking is enabled
				if deferredEnabled {
					// Collect failed articles for deferred verification instead of failing
					deferredMu.Lock()
					for _, art := range failedArticles {
						allDeferredArticles = append(allDeferredArticles, FailedArticleInfo{
							MessageID: art.MessageID,
							Groups:    art.Groups,
						})
					}
					deferredMu.Unlock()

					slog.InfoContext(ctx,
						"Articles deferred for later verification",
						"file", post.FilePath,
						"deferred_count", len(failedArticles),
						"retries_exhausted", p.checkCfg.MaxRePost,
					)

					// Mark as verified (optimistically) - deferred check will update later
					post.mu.Lock()
					post.Status = PostStatusVerified
					post.mu.Unlock()

					postsInFlight.Done()
					p.jobProgress.FinishProgress(post.progress.GetID())

					if post.file != nil {
						if err := post.file.Close(); err != nil {
							slog.WarnContext(ctx, "Error closing file handle", "error", err, "file", post.FilePath)
						}
					}

					post.wg.Done()
					continue
				}

				// Deferred checking not enabled - fail as before
				post.mu.Lock()
				post.Status = PostStatusFailed
				post.Error = fmt.Errorf("failed to verify articles after %d retries", p.checkCfg.MaxRePost)
				post.mu.Unlock()

				// Mark this post as done in queue tracking - it failed permanently
				postsInFlight.Done()

				if post.failed != nil {
					post.failed.Add(1)
				}

				if post.file != nil {
					if cerr := post.file.Close(); cerr != nil {
						slog.WarnContext(ctx, "Error closing file handle on verify failure", "error", cerr, "file", post.FilePath)
					}
				}

				errChan <- fmt.Errorf("failed to verify file %s after %d retries", post.FilePath, p.checkCfg.MaxRePost)

				// Balance the per-file WaitGroup so Post()'s wg.Wait()
				// goroutine can terminate. Done AFTER the (buffered) error
				// send so Post() cannot wake on `done` before the error is
				// observable.
				post.wg.Done()
				return
			} else if errors != nil {
				// This is a safety check - if we have errors but no failed articles, something went wrong
				post.mu.Lock()
				post.Status = PostStatusFailed
				post.Error = fmt.Errorf("verification failed but no articles were marked as failed: %v", errors)
				post.mu.Unlock()

				// Mark this post as done in queue tracking - it failed with unexpected error
				postsInFlight.Done()

				if post.failed != nil {
					post.failed.Add(1)
				}

				if post.file != nil {
					if cerr := post.file.Close(); cerr != nil {
						slog.WarnContext(ctx, "Error closing file handle on verify failure", "error", cerr, "file", post.FilePath)
					}
				}

				errChan <- fmt.Errorf("unexpected error verifying file %s: %v", post.FilePath, errors)

				// Balance the per-file WaitGroup so Post()'s wg.Wait()
				// goroutine can terminate. Done AFTER the (buffered) error
				// send so Post() cannot wake on `done` before the error is
				// observable.
				post.wg.Done()
				return
			}

			// Mark as verified
			post.mu.Lock()
			post.Status = PostStatusVerified
			post.mu.Unlock()

			// Mark this post as done in queue tracking - verification successful
			postsInFlight.Done()

			p.jobProgress.FinishProgress(post.progress.GetID())

			// Close file
			if post.file != nil {
				if err := post.file.Close(); err != nil {
					slog.WarnContext(ctx, "Error closing file handle", "error", err, "file", post.FilePath)
				}
			}

			post.wg.Done()
		}
	}

	// After processing all posts, if there are deferred articles, send a DeferredCheckError
	// This is a non-fatal error that signals the caller to store these for later verification.
	// Guard the send with ctx so a leaked checkLoop cannot block forever if the main
	// goroutine has already returned.
	if len(allDeferredArticles) > 0 {
		slog.InfoContext(ctx, "Sending deferred check error",
			"deferred_articles", len(allDeferredArticles),
			"total_articles", totalArticlesProcessed)
		select {
		case errChan <- &DeferredCheckError{
			FailedArticles: allDeferredArticles,
			TotalArticles:  totalArticlesProcessed,
		}:
		case <-ctx.Done():
		}
	}
}

// addPost adds a file to the posting queue
// displayName is the name to use in the subject (e.g., "Folder/subfolder/file.mp4")
// If displayName is empty, the filename is used
// addRecoveredPost re-posts a file from its existing manifest after a crash:
// it reconstructs the articles (preserving their original Message-IDs/offsets),
// STATs each to skip those already present on the server, and enqueues only the
// missing ones. It never rewrites the manifest. If every article is already
// present the post is enqueued with no articles and completes immediately.
func (p *poster) addRecoveredPost(ctx context.Context, filePath string, fileNumber int, file *os.File, fileInfo os.FileInfo, recs []manifest.ArticleRecord, wg *sync.WaitGroup, failedPosts *atomic.Int64, postQueue chan<- *Post, nzbGen nzb.NZBGenerator, postsInFlight *sync.WaitGroup) error {
	// The manifest does not carry OriginalName/FileNumber, but the NZB
	// generator groups segments by OriginalName and orders files by
	// FileNumber; without them every recovered file collapses into one
	// nameless NZB entry.
	originalName := filepath.Base(filePath)
	all := make([]*article.Article, 0, len(recs))
	for _, r := range recs {
		art := articleFromRecord(r)
		art.OriginalName = originalName
		art.FileNumber = fileNumber
		all = append(all, art)
	}
	missing := p.filterMissing(ctx, all)

	// Articles already on the server are not re-posted, but they are still
	// part of the upload and must appear in the NZB. Only the ones posted now
	// are added by processPost.
	isMissing := make(map[string]struct{}, len(missing))
	for _, art := range missing {
		isMissing[art.MessageID] = struct{}{}
	}
	for _, art := range all {
		if _, ok := isMissing[art.MessageID]; !ok {
			nzbGen.AddArticle(art)
		}
	}

	slog.InfoContext(ctx, "Recovered manifest; re-posting only missing articles",
		"file", filePath, "total", len(all), "missing", len(missing))

	post := &Post{
		FilePath: filePath,
		Articles: missing,
		Status:   PostStatusPending,
		file:     file,
		filesize: fileInfo.Size(),
		wg:       wg,
		failed:   failedPosts,
		progress: p.jobProgress.AddProgress(uuid.New(), filepath.Base(filePath), progress.ProgressTypeUploading, fileInfo.Size()),
	}

	postsInFlight.Add(1)
	select {
	case postQueue <- post:
		return nil
	case <-ctx.Done():
		postsInFlight.Done()
		_ = file.Close()
		return ctx.Err()
	}
}

// filterMissing STATs the articles in batches and returns those that are NOT
// already present on the server, preserving order. On a STAT error the article
// is treated as missing (re-posting an already-present article is idempotent).
// If no verify pool is available, all articles are returned.
func (p *poster) filterMissing(ctx context.Context, articles []*article.Article) []*article.Article {
	if p.verifyPool == nil || len(articles) == 0 {
		return articles
	}

	ids := make([]string, len(articles))
	for i, art := range articles {
		ids[i] = art.MessageID
	}

	missingIDs, err := pool.StatMissing(ctx, p.verifyPool, ids, p.checkCfg.StatBatchSize)
	if err != nil {
		return articles // cancelled sweep: fall back to re-posting everything
	}

	missing := make([]*article.Article, 0, len(articles))
	for _, art := range articles {
		if _, ok := missingIDs[art.MessageID]; ok {
			missing = append(missing, art)
		}
	}
	return missing
}

func (p *poster) addPost(ctx context.Context, filePath string, displayName string, fileNumber int, totalFiles int, wg *sync.WaitGroup, failedPosts *atomic.Int64, postQueue chan<- *Post, nzbGen nzb.NZBGenerator, postsInFlight *sync.WaitGroup) error {
	file, err := os.Open(filePath)
	if err != nil {
		return fmt.Errorf("error opening file: %w", err)
	}

	// Close the file on every path that does not transfer ownership (to the
	// enqueued Post or to addRecoveredPost). Header/Message-ID generation
	// errors otherwise leak one descriptor per affected file.
	owned := true
	defer func() {
		if owned {
			_ = file.Close()
		}
	}()

	fileInfo, err := file.Stat()
	if err != nil {
		return fmt.Errorf("error getting file info: %w", err)
	}

	// Skip empty files - they have no segments to post. Balance the per-file
	// WaitGroup (Post() did wg.Add(len(files)) up front); without this
	// wg.Wait() never completes and Post() hangs forever.
	if fileInfo.Size() == 0 {
		slog.WarnContext(ctx, "Skipping empty file", "path", filePath)
		wg.Done()
		return nil
	}

	// Crash recovery: if a manifest already exists for this file, reuse its exact
	// Message-IDs/offsets instead of regenerating (which would orphan the already
	// posted articles and create duplicate NZB segments). We STAT each planned
	// article and re-post only the ones still missing.
	if p.manifestSink != nil {
		if recs, ok, err := p.manifestSink.ExistingArticles(ctx, filePath); err != nil {
			slog.WarnContext(ctx, "Failed to read existing manifest; regenerating", "file", filePath, "error", err)
		} else if ok {
			owned = false // addRecoveredPost enqueues or closes the file itself
			return p.addRecoveredPost(ctx, filePath, fileNumber, file, fileInfo, recs, wg, failedPosts, postQueue, nzbGen, postsInFlight)
		}
	}

	// Calculate number of segments
	segmentSize := p.cfg.ArticleSizeInBytes
	numSegments := int((fileInfo.Size() + int64(segmentSize) - 1) / int64(segmentSize))
	nxgHeader := nxg.GenerateNXGHeader(int64(numSegments), 0)

	groups := make([]string, 0)

	switch p.cfg.GroupPolicy {
	case config.GroupPolicyEachFile:
		if len(p.cfg.Groups) == 0 {
			return fmt.Errorf("group policy is %q but no newsgroups are configured", config.GroupPolicyEachFile)
		}
		randomGroup := p.cfg.Groups[rand.Intn(len(p.cfg.Groups))]
		groups = append(groups, randomGroup.Name)
	case config.GroupPolicyAll:
		groups = make([]string, 0)
		for _, group := range p.cfg.Groups {
			groups = append(groups, group.Name)
		}
	}

	from, err := article.GenerateFrom()
	if err != nil {
		return fmt.Errorf("error generating from header: %w", err)
	}

	customHeaders := make(map[string]string)
	if len(p.cfg.PostHeaders.CustomHeaders) > 0 {
		for _, v := range p.cfg.PostHeaders.CustomHeaders {
			customHeaders[v.Name] = v.Value
		}
	}

	partType := nxg.PartTypeData
	if par2.IsPar2File(filePath) {
		partType = nxg.PartTypePar2
	}

	// Create articles for each segment
	articles := make([]*article.Article, 0, numSegments)
	for i := range numSegments {
		offset := int64(i) * int64(segmentSize)
		size := int64(segmentSize)
		if offset+size > fileInfo.Size() {
			size = fileInfo.Size() - offset
		}

		partNumber := i + 1

		messageID := ""
		if p.cfg.MessageIDFormat == config.MessageIDFormatRandom {
			msgID, err := article.GenerateMessageID()
			if err != nil {
				return fmt.Errorf("error generating message ID: %w", err)
			}

			messageID = msgID
		} else {
			msgID, err := nxgHeader.GenerateSegmentID(partType, int64(partNumber))
			if err != nil {
				return fmt.Errorf("error generating message ID: %w", err)
			}

			messageID = msgID
		}

		// Use displayName if provided, otherwise fall back to filename
		fileName := filepath.Base(filePath)
		subjectName := fileName
		if displayName != "" {
			subjectName = displayName
		}
		subject := article.GenerateSubject(fileNumber, totalFiles, subjectName, partNumber, numSegments)
		originalSubject := subject

		var fName string

		obfuscationPolicy := p.cfg.ObfuscationPolicy
		if par2.IsPar2File(filePath) {
			obfuscationPolicy = p.cfg.Par2ObfuscationPolicy
		}

		switch obfuscationPolicy {
		case config.ObfuscationPolicyNone:
			fName = fileName
		case config.ObfuscationPolicyFull:
			fName = article.GenerateRandomFilename()
			subject = article.GenerateRandomSubject()
		default:
			hasher := md5.New()
			_, _ = fmt.Fprintf(hasher, "%s%d", fileName, partNumber)
			fName = fmt.Sprintf("%x", hasher.Sum(nil))

			if p.cfg.MessageIDFormat == config.MessageIDFormatRandom {
				hasher := md5.New()
				_, _ = hasher.Write([]byte(subject))
				subject = fmt.Sprintf("%x", hasher.Sum(nil))
			} else {
				subject, err = nxgHeader.GetObfuscatedSubject(partType, int64(partNumber))
				if err != nil {
					return fmt.Errorf("error generating obfuscated subject: %w", err)
				}
			}
		}

		defaultFrom := p.cfg.PostHeaders.DefaultFrom
		if defaultFrom != "" {
			from = defaultFrom
		} else if p.cfg.ObfuscationPolicy == config.ObfuscationPolicyFull {
			from, err = article.GenerateFrom()
			if err != nil {
				return fmt.Errorf("error generating from header: %w", err)
			}
		}

		var xNxgHeader string
		if p.cfg.PostHeaders.AddNXGHeader &&
			p.cfg.ObfuscationPolicy != config.ObfuscationPolicyFull &&
			p.cfg.MessageIDFormat != config.MessageIDFormatNXG {
			xNxgHeader, err = nxgHeader.GetXNxgHeader(
				int64(fileNumber),
				int64(totalFiles),
				fileName,
				partType,
				fileInfo.Size(),
			)
			if err != nil {
				return fmt.Errorf("error generating xNxg header: %w", err)
			}
		}

		var date *time.Time
		if p.cfg.ObfuscationPolicy == config.ObfuscationPolicyFull {
			rd := article.RandomDateWithinLast6Hours()
			date = &rd
		}

		art := article.New(
			messageID,
			subject,
			originalSubject,
			from,
			groups,
			partNumber,
			numSegments,
			fileInfo.Size(),
			fName,
			fileNumber,
			fileName,
			customHeaders,
		)

		if date != nil {
			art.Date = *date
		}

		if xNxgHeader != "" {
			art.XNxgHeader = xNxgHeader
		}

		art.Offset = offset
		art.Size = uint64(size)

		articles = append(articles, art)
	}

	// Record a durable manifest of this file's articles BEFORE any network
	// posting, so an interrupted upload can be resumed/verified with the exact
	// Message-IDs that will be posted. If the manifest cannot be written we fail
	// the file rather than posting without durable recovery data.
	if p.manifestSink != nil {
		if err := p.manifestSink.RecordFile(ctx, filePath, articles); err != nil {
			return fmt.Errorf("recording manifest for %s: %w", filePath, err)
		}
	}

	post := &Post{
		FilePath: filePath,
		Articles: articles,
		Status:   PostStatusPending,
		file:     file,
		filesize: fileInfo.Size(),
		wg:       wg,
		failed:   failedPosts,
		progress: p.jobProgress.AddProgress(uuid.New(), filepath.Base(filePath), progress.ProgressTypeUploading, fileInfo.Size()),
	}

	// Track this post as in-flight until it's sent to the queue
	postsInFlight.Add(1)

	// Use select to safely send to channel and handle context cancellation
	select {
	case postQueue <- post:
		// Successfully sent to queue; the Post now owns the file handle.
		owned = false
		return nil
	case <-ctx.Done():
		// Context canceled, decrement counter since we didn't send. The
		// deferred close releases the file.
		postsInFlight.Done()
		return ctx.Err()
	}
}

// postArticle posts an article to Usenet (reads body from file)
func (p *poster) postArticle(ctx context.Context, article *article.Article, file *os.File) error {
	// Check if we should pause before posting
	if err := pausable.CheckPause(ctx); err != nil {
		return err
	}

	// Read article body
	body := make([]byte, article.Size)
	if _, err := file.ReadAt(body, article.Offset); err != nil {
		return fmt.Errorf("error reading article body: %w", err)
	}

	return p.postArticleWithBody(ctx, article, body)
}

// postArticleWithBody posts an article with pre-read body data
func (p *poster) postArticleWithBody(ctx context.Context, art *article.Article, body []byte) error {
	// Check if we should pause before posting
	if err := pausable.CheckPause(ctx); err != nil {
		return err
	}

	// Delegate to the shared posting primitive so the normal and durable
	// re-post paths build identical headers and apply the same stale-connection
	// retry, throttle and stats accounting.
	return postYenc(ctx, p.uploadPool, p.throttle, p.stats, art, body)
}

// isStaleConnError reports whether err looks like a stale pooled connection:
// the remote server silently closed the TCP socket (broken pipe / connection
// reset) or the connection went half-open and the read deadline fired before
// any response arrived (i/o timeout). In all of these cases the pool discards
// the bad connection, so retrying picks a fresh one.
//
// True context cancellation / deadline exceeded on the caller's postCtx is
// short-circuited at the call site before this is invoked, so any net.Error
// timeout reaching here is a per-socket read deadline rather than the
// per-article envelope.
func isStaleConnError(err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, syscall.EPIPE) || errors.Is(err, syscall.ECONNRESET) {
		return true
	}
	var ne net.Error
	if errors.As(err, &ne) && ne.Timeout() {
		return true
	}
	msg := err.Error()
	return strings.Contains(msg, "broken pipe") ||
		strings.Contains(msg, "connection reset by peer") ||
		strings.Contains(msg, "i/o timeout")
}

// checkArticle checks if an article exists using the check pool
func (p *poster) checkArticle(ctx context.Context, art *article.Article) error {
	// Check if we should pause before checking
	if err := pausable.CheckPause(ctx); err != nil {
		return err
	}

	// Use the dedicated verify pool for article verification
	// v4 Stat only takes messageID (no groups parameter)
	_, err := p.verifyPool.Stat(ctx, art.MessageID)
	if err != nil {
		return fmt.Errorf("article not found: %w", err)
	}

	// Update stats
	p.stats.mu.Lock()
	p.stats.ArticlesChecked++
	p.stats.mu.Unlock()

	return nil
}

// Stats returns posting statistics
func (p *poster) Stats() StatsSnapshot {
	return p.stats.snapshot()
}
