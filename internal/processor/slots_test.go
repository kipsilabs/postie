package processor

import (
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestSlotRunner_NeverExceedsLimit(t *testing.T) {
	s := newSlotRunner()
	var running, peak atomic.Int32
	release := make(chan struct{})
	var wg sync.WaitGroup
	task := func() {
		n := running.Add(1)
		for {
			p := peak.Load()
			if n <= p || peak.CompareAndSwap(p, n) {
				break
			}
		}
		<-release
		running.Add(-1)
		wg.Done()
	}
	wg.Add(3)
	for range 5 { // overlapping fills must not over-claim
		s.fill(3, task)
	}
	time.Sleep(50 * time.Millisecond)
	if got := peak.Load(); got != 3 {
		t.Fatalf("peak concurrency = %d, want 3", got)
	}
	close(release)
	wg.Wait()
}

// A slow task must not prevent the freed slots from picking up new work.
func TestSlotRunner_SlowTaskDoesNotBlockOtherSlots(t *testing.T) {
	s := newSlotRunner()
	slow := make(chan struct{})
	defer close(slow)
	var fastDone atomic.Int32

	s.fill(1, func() { <-slow }) // occupies one of three slots forever
	for range 10 {
		s.fill(3, func() { fastDone.Add(1) })
		time.Sleep(10 * time.Millisecond)
	}
	if got := fastDone.Load(); got < 10 {
		t.Fatalf("fast tasks completed = %d, want >= 10 while one slot is stuck", got)
	}
	if got := s.active.Load(); got != 1 {
		t.Fatalf("active = %d, want 1 (only the slow task)", got)
	}
}

func TestSlotRunner_SignalIsNonBlockingAndCoalesced(t *testing.T) {
	s := newSlotRunner()
	for range 5 {
		s.signal() // must not block with a full buffer
	}
	select {
	case <-s.wake:
	default:
		t.Fatal("expected a pending wake signal")
	}
	select {
	case <-s.wake:
		t.Fatal("signals should coalesce into one")
	default:
	}
}
