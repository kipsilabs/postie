package poster

import (
	"errors"
	"fmt"
	"io"
	"net"
	"syscall"
	"testing"
	"time"
)

// fakeTimeoutErr satisfies net.Error with Timeout()==true, like a wrapped
// os.SyscallError from a TCP read deadline.
type fakeTimeoutErr struct{}

func (fakeTimeoutErr) Error() string   { return "fake: deadline exceeded" }
func (fakeTimeoutErr) Timeout() bool   { return true }
func (fakeTimeoutErr) Temporary() bool { return true }

type fakeNonTimeoutNetErr struct{}

func (fakeNonTimeoutNetErr) Error() string   { return "fake: refused" }
func (fakeNonTimeoutNetErr) Timeout() bool   { return false }
func (fakeNonTimeoutNetErr) Temporary() bool { return false }

func TestIsStaleConnError(t *testing.T) {
	t.Parallel()

	realTimeout := &net.OpError{
		Op:  "read",
		Net: "tcp",
		Err: fakeTimeoutErr{},
	}

	tests := []struct {
		name string
		err  error
		want bool
	}{
		{name: "nil", err: nil, want: false},
		{name: "EPIPE", err: syscall.EPIPE, want: true},
		{name: "ECONNRESET", err: syscall.ECONNRESET, want: true},
		{name: "wrapped EPIPE", err: fmt.Errorf("post: %w", syscall.EPIPE), want: true},
		{name: "wrapped net.Error timeout", err: fmt.Errorf("nntp: %w", realTimeout), want: true},
		{name: "string broken pipe", err: errors.New("write: broken pipe"), want: true},
		{name: "string connection reset", err: errors.New("read: connection reset by peer"), want: true},
		{name: "string i/o timeout", err: errors.New("read tcp 1.2.3.4:42->5.6.7.8:563: i/o timeout"), want: true},
		{name: "net.Error non-timeout", err: fakeNonTimeoutNetErr{}, want: false},
		{name: "io.EOF", err: io.EOF, want: false},
		{name: "generic error", err: errors.New("posting forbidden"), want: false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			if got := isStaleConnError(tt.err); got != tt.want {
				t.Errorf("isStaleConnError(%v) = %v, want %v", tt.err, got, tt.want)
			}
		})
	}
}

// Ensure a real net.Error from a deadline behaves as expected (defense in
// depth against future stdlib changes to error wrapping).
func TestIsStaleConnError_RealDeadline(t *testing.T) {
	t.Parallel()

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Skipf("cannot listen: %v", err)
	}
	defer func() { _ = ln.Close() }()

	conn, err := net.Dial("tcp", ln.Addr().String())
	if err != nil {
		t.Skipf("cannot dial: %v", err)
	}
	defer func() { _ = conn.Close() }()

	_ = conn.SetReadDeadline(time.Now().Add(50 * time.Millisecond))
	buf := make([]byte, 1)
	_, readErr := conn.Read(buf)
	if readErr == nil {
		t.Fatalf("expected read deadline error, got nil")
	}
	if !isStaleConnError(readErr) {
		t.Errorf("isStaleConnError(%v) = false, want true", readErr)
	}
}
