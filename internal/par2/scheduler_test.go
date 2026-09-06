package par2

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/kipsilabs/postie/pkg/fileinfo"
)

// fakeExecutor records how many calls are in flight simultaneously and blocks
// until released, so tests can observe the scheduler's concurrency gate.
type fakeExecutor struct {
	mu          sync.Mutex
	inFlight    int
	maxInFlight int
	release     chan struct{}
	calls       atomic.Int64
}

func newFakeExecutor() *fakeExecutor {
	return &fakeExecutor{release: make(chan struct{})}
}

func (f *fakeExecutor) run(ctx context.Context) ([]string, error) {
	f.calls.Add(1)
	f.mu.Lock()
	f.inFlight++
	if f.inFlight > f.maxInFlight {
		f.maxInFlight = f.inFlight
	}
	f.mu.Unlock()

	defer func() {
		f.mu.Lock()
		f.inFlight--
		f.mu.Unlock()
	}()

	select {
	case <-f.release:
		return []string{"ok"}, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

func (f *fakeExecutor) Create(ctx context.Context, _ []fileinfo.FileInfo) ([]string, error) {
	return f.run(ctx)
}
func (f *fakeExecutor) CreateInDirectory(ctx context.Context, _ []fileinfo.FileInfo, _ string) ([]string, error) {
	return f.run(ctx)
}
func (f *fakeExecutor) CreateSet(ctx context.Context, _ []fileinfo.FileInfo, _, _, _ string) ([]string, error) {
	return f.run(ctx)
}

func TestScheduler_BoundsConcurrency(t *testing.T) {
	const limit = 2
	const total = 8
	sched := NewScheduler(limit)
	fake := newFakeExecutor()
	exec := NewScheduledExecutor(fake, sched)

	var wg sync.WaitGroup
	for i := 0; i < total; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			_, _ = exec.CreateSet(context.Background(), nil, "out", "set", "dir")
		}()
	}

	// Wait until the scheduler is saturated and several tasks are queued.
	deadline := time.After(2 * time.Second)
	for sched.Active() != limit || sched.Queued() < int64(total-limit) {
		select {
		case <-deadline:
			t.Fatalf("scheduler never saturated: active=%d queued=%d", sched.Active(), sched.Queued())
		default:
			time.Sleep(time.Millisecond)
		}
	}

	// Release all work and let it drain.
	close(fake.release)
	wg.Wait()

	if fake.maxInFlight > limit {
		t.Errorf("max concurrent executions = %d, want <= %d", fake.maxInFlight, limit)
	}
	if got := fake.calls.Load(); got != total {
		t.Errorf("executed %d tasks, want %d", got, total)
	}
	if sched.Active() != 0 || sched.Queued() != 0 {
		t.Errorf("after drain active=%d queued=%d, want 0/0", sched.Active(), sched.Queued())
	}
}

func TestScheduler_CancelWhileWaitingDoesNotRun(t *testing.T) {
	sched := NewScheduler(1)
	fake := newFakeExecutor()
	exec := NewScheduledExecutor(fake, sched)

	// Occupy the single slot with a blocking task.
	blockerDone := make(chan struct{})
	go func() {
		_, _ = exec.Create(context.Background(), nil)
		close(blockerDone)
	}()

	// Wait until the blocker holds the slot.
	deadline := time.After(2 * time.Second)
	for sched.Active() != 1 {
		select {
		case <-deadline:
			t.Fatal("blocker never acquired slot")
		default:
			time.Sleep(time.Millisecond)
		}
	}

	callsBefore := fake.calls.Load()

	// A second task whose context is cancelled while waiting must not run.
	ctx, cancel := context.WithCancel(context.Background())
	waiterErr := make(chan error, 1)
	go func() {
		_, err := exec.Create(ctx, nil)
		waiterErr <- err
	}()

	// Ensure it is queued, then cancel.
	for sched.Queued() != 1 {
		select {
		case <-deadline:
			t.Fatal("waiter never queued")
		default:
			time.Sleep(time.Millisecond)
		}
	}
	cancel()

	select {
	case err := <-waiterErr:
		if !errors.Is(err, context.Canceled) {
			t.Errorf("waiter err = %v, want context.Canceled", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("cancelled waiter did not return")
	}

	if got := fake.calls.Load(); got != callsBefore {
		t.Errorf("cancelled task executed: calls went %d -> %d", callsBefore, got)
	}

	// Release the blocker.
	close(fake.release)
	<-blockerDone
}

func TestScheduler_NilSchedulerReturnsInner(t *testing.T) {
	fake := newFakeExecutor()
	if got := NewScheduledExecutor(fake, nil); got != fake {
		t.Errorf("NewScheduledExecutor(inner, nil) = %v, want inner unchanged", got)
	}
}

func TestScheduler_ClampsToOne(t *testing.T) {
	if c := NewScheduler(0).Capacity(); c != 1 {
		t.Errorf("NewScheduler(0).Capacity() = %d, want 1", c)
	}
	if c := NewScheduler(-5).Capacity(); c != 1 {
		t.Errorf("NewScheduler(-5).Capacity() = %d, want 1", c)
	}
}
