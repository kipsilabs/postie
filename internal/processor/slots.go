package processor

import "sync/atomic"

// slotRunner runs tasks on up to a fixed number of concurrent slots. Unlike a
// batch (start N, wait for all N), a freed slot can be refilled immediately, so
// one slow task never idles the other slots.
type slotRunner struct {
	active atomic.Int32
	// wake receives a signal each time a task finishes so the dispatcher can
	// refill the slot without waiting for its next tick.
	wake chan struct{}
}

func newSlotRunner() *slotRunner {
	return &slotRunner{wake: make(chan struct{}, 1)}
}

// fill starts task on every free slot up to limit and returns how many it
// started. The slot is claimed before the goroutine starts, so overlapping
// fill calls can never exceed limit.
func (s *slotRunner) fill(limit int, task func()) int {
	started := 0
	for {
		cur := s.active.Load()
		if int(cur) >= limit {
			return started
		}
		if !s.active.CompareAndSwap(cur, cur+1) {
			continue
		}
		started++
		go func() {
			defer func() {
				s.active.Add(-1)
			}()
			task()
		}()
	}
}

// signal wakes the dispatcher without blocking.
func (s *slotRunner) signal() {
	select {
	case s.wake <- struct{}{}:
	default:
	}
}
