package par2

import (
	"context"
	"sync/atomic"

	"github.com/kipsilabs/postie/pkg/fileinfo"
)

// Scheduler bounds the number of PAR2 operations that run concurrently across
// the whole process. Work submitted while every slot is occupied waits until a
// slot frees up (or its context is cancelled). Because of this gate the
// configured PAR2 memory limit applies per active job instead of being
// multiplied across every simultaneous queue job.
//
// A Scheduler is safe for concurrent use and is intended to be created once and
// shared by all upload jobs through the transfer runtime.
type Scheduler struct {
	sem    chan struct{}
	active atomic.Int64
	queued atomic.Int64
}

// NewScheduler returns a Scheduler that allows at most maxConcurrentJobs PAR2
// operations to run at once. Values below 1 are clamped to 1.
func NewScheduler(maxConcurrentJobs int) *Scheduler {
	if maxConcurrentJobs < 1 {
		maxConcurrentJobs = 1
	}
	return &Scheduler{sem: make(chan struct{}, maxConcurrentJobs)}
}

// Run blocks until a scheduler slot is available, then runs fn and releases the
// slot. If ctx is cancelled while waiting for a slot, Run returns ctx.Err()
// without invoking fn. fn itself should honour ctx so that active work can be
// cancelled too.
func (s *Scheduler) Run(ctx context.Context, fn func() ([]string, error)) ([]string, error) {
	s.queued.Add(1)
	select {
	case <-ctx.Done():
		s.queued.Add(-1)
		return nil, ctx.Err()
	case s.sem <- struct{}{}:
		s.queued.Add(-1)
	}

	s.active.Add(1)
	defer func() {
		s.active.Add(-1)
		<-s.sem
	}()

	return fn()
}

// Active reports the number of PAR2 operations currently running.
func (s *Scheduler) Active() int64 { return s.active.Load() }

// Queued reports the number of PAR2 operations waiting for a slot.
func (s *Scheduler) Queued() int64 { return s.queued.Load() }

// Capacity reports the maximum number of concurrent PAR2 operations.
func (s *Scheduler) Capacity() int { return cap(s.sem) }

// ScheduledExecutor wraps a Par2Executor so that every PAR2 operation runs
// through a shared, process-wide Scheduler. The wrapped executor keeps its
// per-job progress and configuration; only execution is gated.
type ScheduledExecutor struct {
	inner Par2Executor
	sched *Scheduler
}

// NewScheduledExecutor returns inner wrapped so its operations run through
// sched. If sched is nil the inner executor is returned unchanged.
func NewScheduledExecutor(inner Par2Executor, sched *Scheduler) Par2Executor {
	if sched == nil {
		return inner
	}
	return &ScheduledExecutor{inner: inner, sched: sched}
}

func (e *ScheduledExecutor) Create(ctx context.Context, files []fileinfo.FileInfo) ([]string, error) {
	return e.sched.Run(ctx, func() ([]string, error) {
		return e.inner.Create(ctx, files)
	})
}

func (e *ScheduledExecutor) CreateInDirectory(ctx context.Context, files []fileinfo.FileInfo, outputDir string) ([]string, error) {
	return e.sched.Run(ctx, func() ([]string, error) {
		return e.inner.CreateInDirectory(ctx, files, outputDir)
	})
}

func (e *ScheduledExecutor) CreateSet(ctx context.Context, files []fileinfo.FileInfo, outputDir, setName, folderDir string) ([]string, error) {
	return e.sched.Run(ctx, func() ([]string, error) {
		return e.inner.CreateSet(ctx, files, outputDir, setName, folderDir)
	})
}
