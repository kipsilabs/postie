package postie

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/kipsilabs/postie/internal/article"
	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/database"
	"github.com/kipsilabs/postie/internal/mocks"
	"github.com/kipsilabs/postie/internal/poster"
	"github.com/kipsilabs/postie/internal/transferstore"
	"github.com/kipsilabs/postie/internal/verification"
	"go.uber.org/mock/gomock"
)

func TestNewRuntime_Par2SchedulerCapacityFromConfig(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	cfg := mocks.NewMockConfig(ctrl)
	cfg.EXPECT().GetPar2Config(gomock.Any()).Return(&config.Par2Config{MaxConcurrentJobs: 3}, nil)

	rt, err := NewRuntime(context.Background(), cfg, nil, nil, "", nil)
	if err != nil {
		t.Fatalf("NewRuntime: %v", err)
	}
	defer func() { _ = rt.Close() }()

	if got := rt.Par2Scheduler().Capacity(); got != 3 {
		t.Errorf("Par2Scheduler capacity = %d, want 3", got)
	}
}

func TestNewRuntime_DefaultsToOneWhenUnset(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	cfg := mocks.NewMockConfig(ctrl)
	cfg.EXPECT().GetPar2Config(gomock.Any()).Return(&config.Par2Config{MaxConcurrentJobs: 0}, nil)

	rt, err := NewRuntime(context.Background(), cfg, nil, nil, "", nil)
	if err != nil {
		t.Fatalf("NewRuntime: %v", err)
	}
	defer func() { _ = rt.Close() }()

	if got := rt.Par2Scheduler().Capacity(); got != 1 {
		t.Errorf("Par2Scheduler capacity = %d, want 1 (default)", got)
	}
}

func TestRuntime_NilSafe(t *testing.T) {
	var rt *Runtime
	if rt.Par2Scheduler() != nil {
		t.Error("nil Runtime.Par2Scheduler() should return nil")
	}
	if rt.UploadEngine() != nil {
		t.Error("nil Runtime.UploadEngine() should return nil")
	}
	if (rt.Metrics() != RuntimeMetrics{}) {
		t.Errorf("nil Runtime.Metrics() = %+v, want zero value", rt.Metrics())
	}
	if err := rt.Close(); err != nil {
		t.Errorf("nil Runtime.Close() = %v, want nil", err)
	}
}

func TestRuntime_MetricsReflectsPar2Capacity(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	cfg := mocks.NewMockConfig(ctrl)
	cfg.EXPECT().GetPar2Config(gomock.Any()).Return(&config.Par2Config{MaxConcurrentJobs: 4}, nil)

	// nil poolManager => no upload engine; PAR2 scheduler still present.
	rt, err := NewRuntime(context.Background(), cfg, nil, nil, "", nil)
	if err != nil {
		t.Fatalf("NewRuntime: %v", err)
	}
	defer func() { _ = rt.Close() }()

	m := rt.Metrics()
	if m.Par2Capacity != 4 {
		t.Errorf("Par2Capacity = %d, want 4", m.Par2Capacity)
	}
	if m.UploadWorkerCount != 0 {
		t.Errorf("UploadWorkerCount = %d, want 0 (no engine)", m.UploadWorkerCount)
	}
}

func newTestTransferStore(t *testing.T) *transferstore.Store {
	t.Helper()
	ctx := context.Background()
	db, err := database.New(ctx, config.DatabaseConfig{
		DatabaseType: "sqlite",
		DatabasePath: filepath.Join(t.TempDir(), "test.db"),
	})
	if err != nil {
		t.Fatalf("database.New: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	if err := db.GetMigrationRunner().MigrateUp(); err != nil {
		t.Fatalf("MigrateUp: %v", err)
	}
	return transferstore.New(db.DB)
}

func TestRuntime_NewManifestRecorder(t *testing.T) {
	store := newTestTransferStore(t)
	rt := &Runtime{store: store, manifestDir: t.TempDir()}

	if rt.NewManifestRecorder("tid-1") == nil {
		t.Error("NewManifestRecorder with a store should return a recorder")
	}
	if rt.NewManifestRecorder("") != nil {
		t.Error("NewManifestRecorder with empty transferID should return nil")
	}

	var none *Runtime
	if none.NewManifestRecorder("tid-1") != nil {
		t.Error("nil Runtime should return nil recorder")
	}
	if (&Runtime{}).NewManifestRecorder("tid-1") != nil {
		t.Error("Runtime without a store should return nil recorder")
	}
}

func TestRuntime_DurableVerificationEnabled(t *testing.T) {
	if (&Runtime{}).DurableVerificationEnabled() {
		t.Error("Runtime without a verify service should report durable verification disabled")
	}
	var none *Runtime
	if none.DurableVerificationEnabled() {
		t.Error("nil Runtime should report durable verification disabled")
	}
	store := newTestTransferStore(t)
	rt := &Runtime{store: store, verifyService: verification.New(store, nil, nil, verification.Config{}, "w")}
	if !rt.DurableVerificationEnabled() {
		t.Error("Runtime with a verify service should report durable verification enabled")
	}
}

// TestRuntime_DiscardTransferRemovesRowsAndManifests: a cancelled durable job
// used to leave its planned transfer_files rows, any verification failures and
// the manifest directory behind forever (never due, never cleaned).
func TestRuntime_DiscardTransferRemovesRowsAndManifests(t *testing.T) {
	store := newTestTransferStore(t)
	manifestDir := t.TempDir()
	rt := &Runtime{store: store, manifestDir: manifestDir}
	ctx := context.Background()

	rec := rt.NewManifestRecorder("tid-cancel")
	src := filepath.Join(t.TempDir(), "video.mkv")
	if err := rec.RecordFile(ctx, src, []*article.Article{{MessageID: "m1", OriginalName: "video.mkv", PartNumber: 1}}); err != nil {
		t.Fatalf("RecordFile: %v", err)
	}
	if err := store.AddFailure(ctx, transferstore.VerificationFailure{
		TransferID: "tid-cancel", FileID: "f", ArticleIndex: 0, MessageID: "m1",
		State: transferstore.FailurePending, NextAttemptAt: time.Now(),
	}); err != nil {
		t.Fatalf("AddFailure: %v", err)
	}
	if _, err := os.Stat(filepath.Join(manifestDir, "tid-cancel")); err != nil {
		t.Fatalf("manifest dir not created: %v", err)
	}

	if err := rt.DiscardTransfer(ctx, "tid-cancel"); err != nil {
		t.Fatalf("DiscardTransfer: %v", err)
	}

	files, err := store.ListFilesByTransfer(ctx, "tid-cancel")
	if err != nil || len(files) != 0 {
		t.Errorf("transfer_files left: %d (err=%v), want 0", len(files), err)
	}
	if n, _ := store.CountFailures(ctx, "tid-cancel", "f", transferstore.FailurePending); n != 0 {
		t.Errorf("verification_failures left: %d, want 0", n)
	}
	if _, err := os.Stat(filepath.Join(manifestDir, "tid-cancel")); !os.IsNotExist(err) {
		t.Errorf("manifest dir still exists (err=%v), want removed", err)
	}

	// Nil-safe and idempotent.
	var none *Runtime
	if err := none.DiscardTransfer(ctx, "tid-cancel"); err != nil {
		t.Errorf("nil runtime: %v", err)
	}
	if err := rt.DiscardTransfer(ctx, "tid-cancel"); err != nil {
		t.Errorf("second discard: %v", err)
	}
}

func TestRuntime_MetricsIncludeUploadThroughput(t *testing.T) {
	rt := &Runtime{uploadEngine: poster.NewEngine(750_000, 0, 2)}
	rt.uploadEngine.UploadMeter().Record(2048)

	m := rt.Metrics()
	if m.UploadBytes != 2048 {
		t.Errorf("UploadBytes = %d, want 2048", m.UploadBytes)
	}
	if m.UploadSpeedBps <= 0 {
		t.Errorf("UploadSpeedBps = %v, want > 0 right after recording", m.UploadSpeedBps)
	}
}
