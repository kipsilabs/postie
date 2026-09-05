package pausable

import (
	"context"
	"sync"
	"testing"
	"time"
)

func TestPausableContext(t *testing.T) {
	ctx := context.Background()
	pausableCtx := NewContext(ctx)

	// Test initial state
	if pausableCtx.IsPaused() {
		t.Error("Context should not be paused initially")
	}

	// Test pause
	pausableCtx.Pause()
	if !pausableCtx.IsPaused() {
		t.Error("Context should be paused after Pause()")
	}

	// Test resume
	pausableCtx.Resume()
	if pausableCtx.IsPaused() {
		t.Error("Context should not be paused after Resume()")
	}
}

func TestCheckPause(t *testing.T) {
	ctx := context.Background()
	pausableCtx := NewContext(ctx)

	// Test CheckPause when not paused
	err := pausableCtx.CheckPause()
	if err != nil {
		t.Errorf("CheckPause should not return error when not paused: %v", err)
	}

	// Test CheckPause when paused and then resumed
	pausableCtx.Pause()

	// Resume in a goroutine after a short delay
	go func() {
		time.Sleep(50 * time.Millisecond)
		pausableCtx.Resume()
	}()

	// This should block briefly then return without error
	start := time.Now()
	err = pausableCtx.CheckPause()
	duration := time.Since(start)

	if err != nil {
		t.Errorf("CheckPause should not return error after resume: %v", err)
	}
	if duration < 40*time.Millisecond {
		t.Error("CheckPause should have blocked for some time")
	}
}

func TestCheckPauseWithCanceledContext(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	pausableCtx := NewContext(ctx)

	pausableCtx.Pause()

	// Cancel the parent context
	cancel()

	// CheckPause should return the parent context's error
	err := pausableCtx.CheckPause()
	if err != context.Canceled {
		t.Errorf("Expected context.Canceled, got: %v", err)
	}
}

func TestFromContext(t *testing.T) {
	// Test direct pausable context
	ctx := context.Background()
	pausableCtx := NewContext(ctx)

	// Should be able to retrieve from direct context
	retrieved, ok := FromContext(pausableCtx)
	if !ok || retrieved != pausableCtx {
		t.Error("Should be able to retrieve pausable context directly")
	}

	// Test wrapped context (simulating context.WithCancel)
	wrappedCtx, cancel := context.WithCancel(pausableCtx)
	defer cancel()

	retrieved, ok = FromContext(wrappedCtx)
	if !ok || retrieved != pausableCtx {
		t.Error("Should be able to retrieve pausable context from wrapped context")
	}
}

func TestCheckPauseFunction(t *testing.T) {
	ctx := context.Background()
	pausableCtx := NewContext(ctx)

	// Test with regular context (should not error)
	regularCtx := context.Background()
	err := CheckPause(regularCtx)
	if err != nil {
		t.Errorf("CheckPause on regular context should not error: %v", err)
	}

	// Test with pausable context (not paused)
	err = CheckPause(pausableCtx)
	if err != nil {
		t.Errorf("CheckPause on unpaused context should not error: %v", err)
	}

	// Test with wrapped pausable context (paused)
	pausableCtx.Pause()
	wrappedCtx, cancel := context.WithCancel(pausableCtx)
	defer cancel()

	// Resume in a goroutine after a short delay
	go func() {
		time.Sleep(50 * time.Millisecond)
		pausableCtx.Resume()
	}()

	// This should block briefly then return without error
	start := time.Now()
	err = CheckPause(wrappedCtx)
	duration := time.Since(start)

	if err != nil {
		t.Errorf("CheckPause should not return error after resume: %v", err)
	}
	if duration < 40*time.Millisecond {
		t.Error("CheckPause should have blocked for some time")
	}
}

// TestResumeWakesAllWaiters guards against the lost-wakeup bug where Resume
// delivered a single token, so only one of the goroutines blocked in
// CheckPause continued and the rest (upload workers, post loop) stayed
// parked forever, leaving the job "running" with no progress.
func TestResumeWakesAllWaiters(t *testing.T) {
	pc := NewContext(context.Background())
	pc.Pause()

	const waiters = 16
	var started, finished sync.WaitGroup
	started.Add(waiters)
	finished.Add(waiters)
	errs := make(chan error, waiters)
	for i := 0; i < waiters; i++ {
		go func() {
			defer finished.Done()
			started.Done()
			errs <- pc.CheckPause()
		}()
	}
	started.Wait()
	time.Sleep(20 * time.Millisecond) // let every goroutine reach the blocking select

	pc.Resume()

	done := make(chan struct{})
	go func() { finished.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("not all CheckPause waiters returned after a single Resume")
	}
	close(errs)
	for err := range errs {
		if err != nil {
			t.Errorf("CheckPause returned error after resume: %v", err)
		}
	}
}

// TestPauseAfterResumeBlocksAgain ensures a Resume with no waiters does not
// leave a stale wakeup that lets the next CheckPause slip through a pause.
func TestPauseAfterResumeBlocksAgain(t *testing.T) {
	pc := NewContext(context.Background())
	pc.Pause()
	pc.Resume()
	pc.Pause()

	returned := make(chan struct{})
	go func() {
		_ = pc.CheckPause()
		close(returned)
	}()

	select {
	case <-returned:
		t.Fatal("CheckPause returned while paused")
	case <-time.After(50 * time.Millisecond):
	}
	pc.Resume()
	select {
	case <-returned:
	case <-time.After(time.Second):
		t.Fatal("CheckPause did not return after Resume")
	}
}
