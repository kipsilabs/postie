package verification

import (
	"context"
	"testing"

	"github.com/kipsilabs/postie/internal/transferstore"
)

// TestVerifyFile_LargeManifestStormStaysCorrect stresses the bounded-concurrency
// STAT fan-out in VerifyFile: a large manifest is verified with many articles
// missing, so hundreds of goroutines concurrently append to the shared misses
// slice through the semaphore. Run under -race, it asserts no data race / crash
// / deadlock and that exactly the missing articles are persisted.
func TestVerifyFile_LargeManifestStormStaysCorrect(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()

	const n = 400
	writeManifest(t, store, "t", "f", n)

	// Mark every odd-indexed article missing (~half), to exercise the concurrent
	// append path under load, not just the all-present fast path.
	var missing []string
	wantMissing := 0
	for i := range n {
		if i%2 == 1 {
			missing = append(missing, mid(i))
			wantMissing++
		}
	}

	// A small concurrency cap forces heavy contention on the semaphore.
	svc := New(store, newFakeStater(missing...), &fakeReposter{}, Config{}, "w")

	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}

	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerifying {
		t.Errorf("state = %q, want verifying (misses present)", got.VerificationState)
	}
	if pending, _ := store.CountFailures(ctx, "t", "f", transferstore.FailurePending); pending != wantMissing {
		t.Errorf("pending failures = %d, want %d", pending, wantMissing)
	}
}

// TestVerifyFile_AllPresentStormVerifies storms the fan-out with everything
// present: hundreds of concurrent STATs all succeed and the file flips to
// verified with no failures. Guards against races/deadlocks on the success path.
func TestVerifyFile_AllPresentStormVerifies(t *testing.T) {
	store := newTestStore(t)
	ctx := context.Background()

	const n = 400
	writeManifest(t, store, "t", "f", n)

	svc := New(store, newFakeStater(), &fakeReposter{}, Config{}, "w")
	tf, _ := store.GetFile(ctx, "t", "f")
	if err := svc.VerifyFile(ctx, tf); err != nil {
		t.Fatalf("VerifyFile: %v", err)
	}

	got, _ := store.GetFile(ctx, "t", "f")
	if got.VerificationState != transferstore.StateVerified {
		t.Errorf("state = %q, want verified", got.VerificationState)
	}
	if total, _ := store.CountFailures(ctx, "t", "f", ""); total != 0 {
		t.Errorf("failures = %d, want 0", total)
	}
}
