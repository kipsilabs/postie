package backend

import (
	"context"
	"path/filepath"
	"testing"

	"github.com/javi11/postie/internal/config"
	"github.com/javi11/postie/internal/database"
	"github.com/javi11/postie/internal/queue"
)

type fakeJobController struct {
	running   map[string]bool
	cancelled []string
}

func (f *fakeJobController) GetRunningJobs() map[string]bool { return f.running }

func (f *fakeJobController) CancelJob(id string) error {
	f.cancelled = append(f.cancelled, id)
	return nil
}

func TestStopRunningJob_CancelsOnlyWhenRunning(t *testing.T) {
	jc := &fakeJobController{running: map[string]bool{"running-job": true}}

	stopRunningJob(jc, "running-job")
	stopRunningJob(jc, "idle-job")

	if len(jc.cancelled) != 1 || jc.cancelled[0] != "running-job" {
		t.Fatalf("expected only running-job to be cancelled, got %v", jc.cancelled)
	}
}

func newTestQueue(t *testing.T) *queue.Queue {
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
	q, err := queue.New(ctx, db)
	if err != nil {
		t.Fatalf("queue.New: %v", err)
	}
	t.Cleanup(func() { _ = q.Close() })
	return q
}

func TestRemoveFromQueue_RemovesPendingItemAndEmitsEvent(t *testing.T) {
	q := newTestQueue(t)
	ctx := context.Background()
	if err := q.AddFile(ctx, filepath.Join(t.TempDir(), "a.bin"), 10); err != nil {
		t.Fatalf("AddFile: %v", err)
	}
	items, err := q.GetQueueItems(queue.PaginationParams{Page: 1, Limit: 10})
	if err != nil || len(items.Items) != 1 {
		t.Fatalf("GetQueueItems: items=%d err=%v", len(items.Items), err)
	}

	var events []string
	app := &App{
		queue:           q,
		isWebMode:       true,
		webEventEmitter: func(eventType string, _ any) { events = append(events, eventType) },
	}

	if err := app.RemoveFromQueue(items.Items[0].ID); err != nil {
		t.Fatalf("RemoveFromQueue: %v", err)
	}

	after, err := q.GetQueueItems(queue.PaginationParams{Page: 1, Limit: 10})
	if err != nil {
		t.Fatalf("GetQueueItems: %v", err)
	}
	if len(after.Items) != 0 {
		t.Fatalf("expected queue to be empty, got %d items", len(after.Items))
	}
	if len(events) != 1 || events[0] != "queue-updated" {
		t.Fatalf("expected one queue-updated event, got %v", events)
	}
}
