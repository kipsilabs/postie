package poster

import (
	"sync"
	"time"
)

// uploadWindow is how far back Speed looks. Ten one-second buckets smooth
// per-article burstiness without hiding a stall for long.
const uploadWindow = 10

// UploadSnapshot is a point-in-time view of bytes handed to the NNTP pool.
type UploadSnapshot struct {
	BytesUploaded int64
	Speed         float64 // bytes/sec over the trailing window; 0 when idle
	AvgSpeed      float64 // bytes/sec since the first recorded byte
}

// UploadMeter counts article payload bytes posted through an Engine. nntppool
// only reports bytes it reads from the wire, which for a poster is a few
// hundred bytes of status lines per article, so upload throughput has to be
// measured on this side.
type UploadMeter struct {
	mu          sync.Mutex
	total       int64
	first       time.Time
	buckets     [uploadWindow]int64
	bucketEpoch int64 // unix second of buckets[bucketEpoch % uploadWindow]
	now         func() time.Time
}

func NewUploadMeter() *UploadMeter {
	return &UploadMeter{now: time.Now}
}

// Record adds n payload bytes at the current instant. Safe on a nil meter.
func (m *UploadMeter) Record(n int64) {
	if m == nil || n <= 0 {
		return
	}
	m.mu.Lock()
	defer m.mu.Unlock()

	t := m.now()
	if m.first.IsZero() {
		m.first = t
	}
	m.advance(t.Unix())
	m.total += n
	m.buckets[m.bucketEpoch%uploadWindow] += n
}

// Snapshot returns current totals and rates. Safe on a nil meter.
func (m *UploadMeter) Snapshot() UploadSnapshot {
	if m == nil {
		return UploadSnapshot{}
	}
	m.mu.Lock()
	defer m.mu.Unlock()

	t := m.now()
	m.advance(t.Unix())

	var windowBytes int64
	for _, b := range m.buckets {
		windowBytes += b
	}
	snap := UploadSnapshot{
		BytesUploaded: m.total,
		Speed:         float64(windowBytes) / uploadWindow,
	}
	if !m.first.IsZero() {
		if elapsed := t.Sub(m.first).Seconds(); elapsed > 0 {
			snap.AvgSpeed = float64(m.total) / elapsed
		}
	}
	return snap
}

// advance rolls the ring forward to second `sec`, zeroing buckets that fell out
// of the window. Caller holds mu.
func (m *UploadMeter) advance(sec int64) {
	if m.bucketEpoch == 0 {
		m.bucketEpoch = sec
		return
	}
	gap := sec - m.bucketEpoch
	if gap <= 0 {
		return
	}
	if gap >= uploadWindow {
		m.buckets = [uploadWindow]int64{}
	} else {
		for i := int64(1); i <= gap; i++ {
			m.buckets[(m.bucketEpoch+i)%uploadWindow] = 0
		}
	}
	m.bucketEpoch = sec
}
