package par2

import (
	"log/slog"
	"os"
	"testing"
)

// The par2go encoder logs its selected GF16 SIMD method, stride, chunk length
// and thread count at Debug level right before handing work to the ParPar C
// kernel. Those values are the only clue left behind when the C kernel
// segfaults on a CI runner (a crash on a C++ thread produces no Go traceback),
// so surface them in the test output.
func TestMain(m *testing.M) {
	slog.SetDefault(slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelDebug})))
	os.Exit(m.Run())
}
