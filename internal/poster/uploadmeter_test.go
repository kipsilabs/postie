package poster

import (
	"context"
	"testing"
	"time"

	"github.com/kipsilabs/postie/internal/article"
	"github.com/kipsilabs/postie/internal/mocks"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/mock/gomock"
)

func TestUploadMeter_TotalsRecordedBytes(t *testing.T) {
	m := NewUploadMeter()
	m.Record(100)
	m.Record(50)

	snap := m.Snapshot()
	assert.Equal(t, int64(150), snap.BytesUploaded)
}

func TestUploadMeter_SpeedReflectsRecentWindowOnly(t *testing.T) {
	now := time.Unix(1_000_000, 0)
	m := NewUploadMeter()
	m.now = func() time.Time { return now }

	// 1 MiB/s for 5 seconds, then silence for 20 seconds.
	for i := 0; i < 5; i++ {
		m.Record(1 << 20)
		now = now.Add(time.Second)
	}
	assert.InDelta(t, float64(1<<20), m.Snapshot().Speed, float64(1<<20)*0.6,
		"speed while actively uploading should be near 1 MiB/s")

	now = now.Add(20 * time.Second)
	assert.Equal(t, float64(0), m.Snapshot().Speed, "speed must decay to 0 once the window has passed")
	assert.Equal(t, int64(5<<20), m.Snapshot().BytesUploaded, "totals must survive the window")
}

func TestUploadMeter_AverageSinceFirstByte(t *testing.T) {
	now := time.Unix(1_000_000, 0)
	m := NewUploadMeter()
	m.now = func() time.Time { return now }

	m.Record(1000)
	now = now.Add(10 * time.Second)
	m.Record(1000)

	assert.InDelta(t, 200.0, m.Snapshot().AvgSpeed, 1.0)
}

func TestUploadMeter_NilSafe(t *testing.T) {
	var m *UploadMeter
	m.Record(10)
	assert.Equal(t, UploadSnapshot{}, m.Snapshot())
}

func TestEngineMetrics_IncludeUploadTotals(t *testing.T) {
	eng := NewEngine(750_000, 0, 4)
	eng.UploadMeter().Record(4096)

	assert.Equal(t, int64(4096), eng.Metrics().UploadBytes)

	var nilEngine *Engine
	assert.Nil(t, nilEngine.UploadMeter())
	assert.Equal(t, int64(0), nilEngine.Metrics().UploadBytes)
}

func TestPostYenc_RecordsBodyBytesOnSuccessOnly(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	mockPool := mocks.NewMockNNTPClient(ctrl)
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(nil, nil).Times(1)
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(nil, context.DeadlineExceeded).Times(1)

	meter := NewUploadMeter()
	art := &article.Article{MessageID: "<x@test>", Groups: []string{"alt.test"}, Size: 4}

	require.NoError(t, postYenc(context.Background(), mockPool, nil, nil, meter, art, []byte("body")))
	require.Error(t, postYenc(context.Background(), mockPool, nil, nil, meter, art, []byte("body")))

	assert.Equal(t, int64(4), meter.Snapshot().BytesUploaded)
}
