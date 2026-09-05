package pausable

import (
	"context"
	"sync"
	"time"
)

// contextKey is used to store the pausable context in context.Value
type contextKey int

const pausableContextKey contextKey = iota

// Context wraps a context.Context and allows pausing/resuming operations
type Context struct {
	parent context.Context
	paused bool
	// resumeCh is created on Pause and closed on Resume. Closing broadcasts
	// to every goroutine blocked in CheckPause; a single token would wake only
	// one of them and leave the rest parked forever.
	resumeCh chan struct{}
	mu       sync.RWMutex
}

// NewContext creates a new pausable context that stores itself in the context chain
func NewContext(parent context.Context) *Context {
	pc := &Context{
		parent: parent,
		paused: false,
	}

	// Store reference to self in the context chain
	pc.parent = context.WithValue(parent, pausableContextKey, pc)

	return pc
}

// Deadline returns the parent context's deadline
func (pc *Context) Deadline() (deadline time.Time, ok bool) {
	return pc.parent.Deadline()
}

// Done returns a channel that's closed when the context is canceled or the parent is done
func (pc *Context) Done() <-chan struct{} {
	return pc.parent.Done()
}

// Err returns the parent context's error
func (pc *Context) Err() error {
	return pc.parent.Err()
}

// Value returns the parent context's value
func (pc *Context) Value(key any) any {
	return pc.parent.Value(key)
}

// Pause pauses the context - operations should call CheckPause() to respect this
func (pc *Context) Pause() {
	pc.mu.Lock()
	defer pc.mu.Unlock()

	if !pc.paused {
		pc.paused = true
		pc.resumeCh = make(chan struct{})
	}
}

// Resume resumes the context and wakes every operation blocked in CheckPause
func (pc *Context) Resume() {
	pc.mu.Lock()
	defer pc.mu.Unlock()

	if pc.paused {
		pc.paused = false
		close(pc.resumeCh)
		pc.resumeCh = nil
	}
}

// CheckPause blocks if the context is paused until it's resumed or the parent context is canceled
func (pc *Context) CheckPause() error {
	pc.mu.RLock()
	paused, resumeCh := pc.paused, pc.resumeCh
	pc.mu.RUnlock()

	if !paused {
		return nil
	}

	// resumeCh was captured under the lock, so a Resume that races with this
	// call closes the same channel we wait on and the wakeup cannot be lost.
	select {
	case <-resumeCh:
		return nil
	case <-pc.parent.Done():
		return pc.parent.Err()
	}
}

// IsPaused returns whether the context is currently paused
func (pc *Context) IsPaused() bool {
	pc.mu.RLock()
	defer pc.mu.RUnlock()
	return pc.paused
}

// FromContext retrieves the pausable context from any context in the chain
// This works even if the context has been wrapped by context.WithCancel, context.WithTimeout, etc.
func FromContext(ctx context.Context) (*Context, bool) {
	if pc, ok := ctx.(*Context); ok {
		return pc, true
	}

	if pc, ok := ctx.Value(pausableContextKey).(*Context); ok {
		return pc, true
	}

	return nil, false
}

// CheckPause is a convenience function that checks if any context in the chain is pausable and paused
func CheckPause(ctx context.Context) error {
	if pc, ok := FromContext(ctx); ok {
		return pc.CheckPause()
	}

	return nil
}
