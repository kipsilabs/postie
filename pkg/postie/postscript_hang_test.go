package postie

import (
	"context"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/kipsilabs/postie/internal/config"
)

// A script that leaves a background child holding its stdout open must not
// block the caller past the timeout (plus WaitDelay).
func TestRunPostUploadScript_BackgroundChildDoesNotHang(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("uses sh")
	}
	cfg := config.PostUploadScriptConfig{
		Enabled: true,
		Command: "sleep 30 & sleep 30",
		Timeout: config.Duration("200ms"),
	}
	done := make(chan error, 1)
	go func() {
		done <- runPostUploadScript(context.Background(), cfg, nil, filepath.Join(t.TempDir(), "a.nzb"), "/src/a", "id")
	}()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("expected a timeout error")
		}
	case <-time.After(15 * time.Second):
		t.Fatal("runPostUploadScript blocked on a lingering child process")
	}
}
