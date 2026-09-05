package progress

import (
	"math"
	"testing"
	"time"

	"github.com/google/uuid"
)

func newClockedProgress(t *testing.T, total int64, now *time.Time) *progress {
	t.Helper()
	job := NewProgressJob("job")
	t.Cleanup(job.Close)
	p, ok := job.AddProgress(uuid.New(), "file.bin", ProgressTypeUploading, total).(*progress)
	if !ok {
		t.Fatal("AddProgress did not return *progress")
	}
	p.clock = func() time.Time { return *now }
	return p
}

func assertClose(t *testing.T, name string, got, want float64) {
	t.Helper()
	if math.Abs(got-want) > 0.01 {
		t.Fatalf("%s = %v, want %v", name, got, want)
	}
}

func TestSpeedExcludesTimeWaitingForFirstByte(t *testing.T) {
	now := time.Unix(1000, 0)
	p := newClockedProgress(t, 4096, &now)

	now = now.Add(30 * time.Second)
	p.UpdateProgress(1024)
	now = now.Add(1 * time.Second)
	p.UpdateProgress(1024)

	assertClose(t, "KBsPerSecond", p.GetState().KBsPerSecond, 2.0)
}

func TestSpeedExcludesPausedTime(t *testing.T) {
	now := time.Unix(1000, 0)
	p := newClockedProgress(t, 4096, &now)

	p.UpdateProgress(1024)
	now = now.Add(1 * time.Second)
	p.SetPaused(true)
	now = now.Add(10 * time.Second)
	p.SetPaused(false)
	now = now.Add(1 * time.Second)
	p.UpdateProgress(1024)

	assertClose(t, "KBsPerSecond", p.GetState().KBsPerSecond, 1.0)
}

func TestSecondsLeftUsesActiveSpeed(t *testing.T) {
	now := time.Unix(1000, 0)
	p := newClockedProgress(t, 4096, &now)

	now = now.Add(30 * time.Second)
	p.UpdateProgress(1024)
	now = now.Add(1 * time.Second)
	p.UpdateProgress(1024)

	assertClose(t, "SecondsLeft", p.GetState().SecondsLeft, 1.0)
}

func TestSpeedIsZeroBeforeFirstByte(t *testing.T) {
	now := time.Unix(1000, 0)
	p := newClockedProgress(t, 4096, &now)
	now = now.Add(5 * time.Second)

	s := p.GetState()
	assertClose(t, "KBsPerSecond", s.KBsPerSecond, 0)
	assertClose(t, "SecondsLeft", s.SecondsLeft, 0)
}

func TestPauseBeforeFirstByteDoesNotSkewSpeed(t *testing.T) {
	now := time.Unix(1000, 0)
	p := newClockedProgress(t, 4096, &now)

	p.SetPaused(true)
	now = now.Add(10 * time.Second)
	p.SetPaused(false)
	now = now.Add(5 * time.Second)
	p.UpdateProgress(1024)
	now = now.Add(1 * time.Second)
	p.UpdateProgress(1024)

	assertClose(t, "KBsPerSecond", p.GetState().KBsPerSecond, 2.0)
}
