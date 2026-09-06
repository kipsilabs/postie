package poster

import (
	"context"
	"sync"
	"sync/atomic"
	"time"

	"golang.org/x/sync/semaphore"
)

// Memory-budget sizing constants. See ComputeEngineBudget.
const (
	// articleOverheadBytes accounts for headers, yEnc framing slack and the
	// rapidyenc working buffers beyond the raw + encoded article body.
	articleOverheadBytes int64 = 256 * 1024
	// minAutoBudgetBytes / maxAutoBudgetBytes clamp the automatically derived
	// upload-buffer budget.
	minAutoBudgetBytes int64 = 64 * 1024 * 1024
	maxAutoBudgetBytes int64 = 512 * 1024 * 1024
)

// EngineBudget is the resolved set of process-wide upload limits.
type EngineBudget struct {
	// PerArticleBytes is the memory reserved for a single in-flight article
	// (raw body + estimated encoded size + fixed overhead).
	PerArticleBytes int64
	// BudgetBytes is the total buffer memory the engine may reserve at once.
	BudgetBytes int64
	// WorkerCount is the maximum number of articles posted concurrently.
	WorkerCount int64
}

// estimatedYencBytes approximates the encoded size of a raw article body. yEnc
// adds roughly 2-3% plus line endings; 5% is a safe upper bound.
func estimatedYencBytes(articleSize int64) int64 {
	return articleSize + articleSize/20
}

// ComputeEngineBudget resolves the process-wide upload-buffer budget and worker
// count from the configured article size, an explicit buffer limit (0 = auto),
// and the total connection capacity (sum of max_connections * effective
// inflight across upload providers).
//
//	per_article  = article_size + estimated_yenc_size + 256 KiB
//	auto_budget  = clamp(per_article * conn_capacity, 64MiB, 512MiB)
//	worker_count = conn_capacity
//
// WorkerCount always equals the connection capacity so posting concurrency
// matches what the NNTP pool can actually drive; the byte budget bounds only
// the in-flight buffer memory. When an explicit (or clamped) budget covers
// fewer than WorkerCount articles, the memory semaphore throttles effective
// concurrency on its own — deriving WorkerCount from the budget as well used to
// cap throughput at ~37 articles regardless of connections (issue #168).
//
// The result guarantees BudgetBytes >= PerArticleBytes and WorkerCount >= 1 so
// callers can always make forward progress.
func ComputeEngineBudget(articleSize uint64, bufferLimit int64, connCapacity int) EngineBudget {
	if articleSize == 0 {
		articleSize = 768 * 1024
	}
	if connCapacity < 1 {
		connCapacity = 1
	}

	raw := int64(articleSize)
	perArticle := raw + estimatedYencBytes(raw) + articleOverheadBytes

	budget := bufferLimit
	if budget <= 0 {
		// Automatic sizing: enough for one in-flight article per connection,
		// within the clamp.
		budget = perArticle * int64(connCapacity)
		if budget < minAutoBudgetBytes {
			budget = minAutoBudgetBytes
		}
		if budget > maxAutoBudgetBytes {
			budget = maxAutoBudgetBytes
		}
	}

	// Always allow at least one in-flight article, even if an explicit limit was
	// set smaller than a single article.
	if budget < perArticle {
		budget = perArticle
	}

	return EngineBudget{
		PerArticleBytes: perArticle,
		BudgetBytes:     budget,
		WorkerCount:     int64(connCapacity),
	}
}

// Engine is the process-wide upload resource owner. It bounds the number of
// articles posted concurrently (worker slots) and the total memory reserved for
// in-flight article buffers (buffer budget), independent of how many queue jobs
// are active. One Engine is shared by every job through the transfer runtime.
//
// All methods are safe for concurrent use and safe to call on a nil *Engine, in
// which case they are no-ops (preserving standalone behaviour when no engine is
// injected).
type Engine struct {
	budget   EngineBudget
	workers  *semaphore.Weighted
	memory   *semaphore.Weighted
	activeWk atomic.Int64
	queuedWk atomic.Int64
	reserved atomic.Int64

	// Engine-wide upload throttle, lazily created by SharedThrottle so every
	// posting path sharing this engine shares one token bucket.
	throttleOnce sync.Once
	throttle     *Throttle

	meter *UploadMeter
}

// UploadMeter returns the engine-wide upload byte meter. Nil on a nil engine.
func (e *Engine) UploadMeter() *UploadMeter {
	if e == nil {
		return nil
	}
	return e.meter
}

// SharedThrottle lazily creates (first caller's rate wins) and returns the
// engine-wide upload throttle for rate bytes/sec. Per-job posters and the
// durable re-poster all post through the same engine; sharing one token bucket
// makes the configured rate bound AGGREGATE egress instead of applying once
// per instance (which allowed ~N× the configured rate when re-posts ran
// alongside uploads). Returns nil when rate <= 0 (no throttling). A nil engine
// falls back to a private throttle, preserving standalone behaviour.
func (e *Engine) SharedThrottle(rate int64) *Throttle {
	if rate <= 0 {
		return nil
	}
	if e == nil {
		return NewThrottle(rate, time.Second)
	}
	e.throttleOnce.Do(func() {
		e.throttle = NewThrottle(rate, time.Second)
	})
	return e.throttle
}

// NewEngine builds an Engine sized for the given article size, explicit buffer
// limit (0 = auto), and total upload connection capacity.
func NewEngine(articleSize uint64, bufferLimit int64, connCapacity int) *Engine {
	b := ComputeEngineBudget(articleSize, bufferLimit, connCapacity)
	return &Engine{
		budget:  b,
		workers: semaphore.NewWeighted(b.WorkerCount),
		memory:  semaphore.NewWeighted(b.BudgetBytes),
		meter:   NewUploadMeter(),
	}
}

// Budget returns the resolved limits.
func (e *Engine) Budget() EngineBudget {
	if e == nil {
		return EngineBudget{}
	}
	return e.budget
}

// PerArticleBytes is the reservation size for a single in-flight article, or 0
// when no engine is configured.
func (e *Engine) PerArticleBytes() int64 {
	if e == nil {
		return 0
	}
	return e.budget.PerArticleBytes
}

// ReserveBuffer blocks until n bytes of buffer budget are available (or ctx is
// cancelled), then records the reservation. Returns ctx.Err() if cancelled
// while waiting. A nil engine or non-positive n is a no-op.
func (e *Engine) ReserveBuffer(ctx context.Context, n int64) error {
	if e == nil || n <= 0 {
		return nil
	}
	if err := e.memory.Acquire(ctx, n); err != nil {
		return err
	}
	e.reserved.Add(n)
	return nil
}

// ReleaseBuffer returns n bytes of buffer budget. Must be paired with a prior
// successful ReserveBuffer of the same size. A nil engine or non-positive n is
// a no-op.
func (e *Engine) ReleaseBuffer(n int64) {
	if e == nil || n <= 0 {
		return
	}
	e.memory.Release(n)
	e.reserved.Add(-n)
}

// AcquireWorker blocks until an upload worker slot is free (or ctx is
// cancelled). Returns ctx.Err() if cancelled while waiting. A nil engine is a
// no-op.
func (e *Engine) AcquireWorker(ctx context.Context) error {
	if e == nil {
		return nil
	}
	e.queuedWk.Add(1)
	err := e.workers.Acquire(ctx, 1)
	e.queuedWk.Add(-1)
	if err != nil {
		return err
	}
	e.activeWk.Add(1)
	return nil
}

// ReleaseWorker frees a worker slot acquired by AcquireWorker. A nil engine is
// a no-op.
func (e *Engine) ReleaseWorker() {
	if e == nil {
		return
	}
	e.activeWk.Add(-1)
	e.workers.Release(1)
}

// Metrics is a point-in-time snapshot of engine activity for observability.
type Metrics struct {
	ActiveWorkers  int64
	QueuedWorkers  int64
	WorkerCount    int64
	ReservedBytes  int64
	BudgetBytes    int64
	PerArticleByte int64
	UploadBytes    int64
	UploadSpeed    float64
	UploadAvgSpeed float64
}

// Metrics returns a snapshot of current engine activity. Safe on a nil engine.
func (e *Engine) Metrics() Metrics {
	if e == nil {
		return Metrics{}
	}
	up := e.meter.Snapshot()
	return Metrics{
		ActiveWorkers:  e.activeWk.Load(),
		QueuedWorkers:  e.queuedWk.Load(),
		WorkerCount:    e.budget.WorkerCount,
		ReservedBytes:  e.reserved.Load(),
		BudgetBytes:    e.budget.BudgetBytes,
		PerArticleByte: e.budget.PerArticleBytes,
		UploadBytes:    up.BytesUploaded,
		UploadSpeed:    up.Speed,
		UploadAvgSpeed: up.AvgSpeed,
	}
}
