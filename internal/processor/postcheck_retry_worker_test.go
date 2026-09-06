package processor

import (
	"context"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/javi11/nntppool/v4"
	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/mocks"
	"github.com/kipsilabs/postie/internal/queue"
	"go.uber.org/mock/gomock"
)

// fakeQueue is a hand-rolled implementation of postCheckQueue for tests.
type fakeQueue struct {
	articles       []queue.PendingArticleCheck
	verified       []int64
	failed         []int64
	retried        []int64
	getErr         error
	verifyErr      error
	failErr        error
	retryErr       error
	countTotal     int
	countPend      int
	countFailed    int
	countErr       error
	statusErr      error
	statusSet      string // last status passed to UpdateCompletedItemVerificationStatus
	statusSetCount int    // number of times UpdateCompletedItemVerificationStatus was called
}

func (f *fakeQueue) GetArticlesForCheck(_ context.Context, limit int) ([]queue.PendingArticleCheck, error) {
	if f.getErr != nil {
		return nil, f.getErr
	}
	if len(f.articles) <= limit {
		return f.articles, nil
	}
	return f.articles[:limit], nil
}

func (f *fakeQueue) MarkArticleVerified(_ context.Context, id int64) error {
	f.verified = append(f.verified, id)
	return f.verifyErr
}

func (f *fakeQueue) MarkArticleCheckFailed(_ context.Context, id int64) error {
	f.failed = append(f.failed, id)
	return f.failErr
}

func (f *fakeQueue) UpdateArticleCheckRetry(_ context.Context, id int64, _ int, _ time.Time) error {
	f.retried = append(f.retried, id)
	return f.retryErr
}

func (f *fakeQueue) GetPendingCheckCountForItem(_ context.Context, _ string) (total int, pending int, failed int, err error) {
	return f.countTotal, f.countPend, f.countFailed, f.countErr
}

func (f *fakeQueue) UpdateCompletedItemVerificationStatus(_ context.Context, _ string, status string) error {
	f.statusSet = status
	f.statusSetCount++
	return f.statusErr
}

// makeEnabled returns a pointer to a bool (helper for config.PostCheck.Enabled).
func makeEnabled(b bool) *bool { return &b }

// makeArticles builds n dummy PendingArticleCheck entries.
func makeArticles(n int, retryCount int) []queue.PendingArticleCheck {
	articles := make([]queue.PendingArticleCheck, n)
	for i := range articles {
		articles[i] = queue.PendingArticleCheck{
			ID:              int64(i + 1),
			CompletedItemID: fmt.Sprintf("item-%d", i+1),
			MessageID:       fmt.Sprintf("<msg-%d@test>", i+1),
			Groups:          `["alt.binaries.test"]`,
			Status:          "pending",
			RetryCount:      retryCount,
		}
	}
	return articles
}

// newWorker builds a PostCheckRetryWorker with sensible test defaults.
func newWorker(ctx context.Context, q postCheckQueue, pool *mocks.MockNNTPClient, batchSize int, maxRetries int) *PostCheckRetryWorker {
	enabled := makeEnabled(true)
	cfg := config.PostCheck{
		Enabled:               enabled,
		DeferredCheckInterval: config.Duration("1m"),
		DeferredCheckDelay:    config.Duration("5m"),
		DeferredMaxBackoff:    config.Duration("1h"),
		DeferredMaxRetries:    maxRetries,
		DeferredBatchSize:     batchSize,
	}
	w := NewPostCheckRetryWorker(ctx, q, pool, cfg, nil)
	return w
}

func TestProcessRetries(t *testing.T) {
	t.Run("empty queue returns false", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		q := &fakeQueue{articles: []queue.PendingArticleCheck{}}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		w := newWorker(context.Background(), q, mockPool, 3, 3)

		got := w.processRetries()
		if got {
			t.Error("expected false for empty queue, got true")
		}
	})

	t.Run("partial batch returns false", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		articles := makeArticles(2, 0)
		q := &fakeQueue{articles: articles, countPend: 1}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(nil)).AnyTimes()
		w := newWorker(context.Background(), q, mockPool, 3, 5)

		got := w.processRetries()
		if got {
			t.Error("expected false for partial batch (2 < batchSize 3), got true")
		}
	})

	t.Run("full batch returns true", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		articles := makeArticles(2, 0)
		q := &fakeQueue{articles: articles, countTotal: 2, countPend: 0}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(nil)).Times(1)
		w := newWorker(context.Background(), q, mockPool, 2, 5)

		got := w.processRetries()
		if !got {
			t.Error("expected true for full batch (2 == batchSize 2), got false")
		}
	})

	t.Run("verified articles marked verified", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		articles := makeArticles(1, 0)
		q := &fakeQueue{articles: articles, countTotal: 1, countPend: 0}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(nil)).Times(1)
		w := newWorker(context.Background(), q, mockPool, 10, 5)

		w.processRetries()

		if len(q.verified) != 1 || q.verified[0] != articles[0].ID {
			t.Errorf("expected article %d to be marked verified, got verified=%v", articles[0].ID, q.verified)
		}
		if len(q.failed) != 0 {
			t.Errorf("expected no failed marks, got %v", q.failed)
		}
	})

	t.Run("failed STAT below maxRetries schedules retry", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		articles := makeArticles(1, 0) // retryCount=0, maxRetries=3 → newRetryCount=1 < 3
		q := &fakeQueue{articles: articles, countTotal: 1, countPend: 1}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(map[string]error{articles[0].MessageID: errors.New("not found")})).Times(1)
		w := newWorker(context.Background(), q, mockPool, 10, 3)

		w.processRetries()

		if len(q.retried) != 1 || q.retried[0] != articles[0].ID {
			t.Errorf("expected article %d to be scheduled for retry, got retried=%v", articles[0].ID, q.retried)
		}
		if len(q.failed) != 0 {
			t.Errorf("expected no failed marks, got %v", q.failed)
		}
	})

	t.Run("failed STAT at maxRetries marks failed", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		// retryCount=2, maxRetries=3 → newRetryCount=3 >= 3 → mark failed
		articles := makeArticles(1, 2)
		q := &fakeQueue{articles: articles, countTotal: 1, countPend: 0, countFailed: 1}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(map[string]error{articles[0].MessageID: errors.New("not found")})).Times(1)
		w := newWorker(context.Background(), q, mockPool, 10, 3)

		w.processRetries()

		if len(q.failed) != 1 || q.failed[0] != articles[0].ID {
			t.Errorf("expected article %d to be marked failed, got failed=%v", articles[0].ID, q.failed)
		}
		if len(q.retried) != 0 {
			t.Errorf("expected no retry schedules, got %v", q.retried)
		}
	})

	t.Run("context cancel mid-batch returns false", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		ctx, cancel := context.WithCancel(context.Background())
		cancel() // cancel immediately

		articles := makeArticles(2, 0)
		q := &fakeQueue{articles: articles}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		// With cancelled ctx, the worker should detect ctx.Err() before processing articles
		// StatMany may or may not be called depending on timing, so allow any calls
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(nil)).AnyTimes()
		w := newWorker(ctx, q, mockPool, 2, 5)

		got := w.processRetries()
		if got {
			t.Error("expected false when context cancelled, got true")
		}
	})

	t.Run("queue error returns false", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		q := &fakeQueue{getErr: errors.New("db error")}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		w := newWorker(context.Background(), q, mockPool, 10, 5)

		got := w.processRetries()
		if got {
			t.Error("expected false on queue error, got true")
		}
	})

	t.Run("onStatusChanged called when all articles verified", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		articles := makeArticles(1, 0)
		q := &fakeQueue{articles: articles, countTotal: 1, countPend: 0, countFailed: 0}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(nil)).Times(1)

		called := false
		enabled := makeEnabled(true)
		cfg := config.PostCheck{
			Enabled:               enabled,
			DeferredCheckInterval: config.Duration("1m"),
			DeferredCheckDelay:    config.Duration("5m"),
			DeferredMaxBackoff:    config.Duration("1h"),
			DeferredMaxRetries:    5,
			DeferredBatchSize:     10,
		}
		w := NewPostCheckRetryWorker(context.Background(), q, mockPool, cfg, func() { called = true })

		w.processRetries()

		if !called {
			t.Error("expected onStatusChanged to be called after all articles verified, but it was not")
		}
		if q.statusSet != "verified" {
			t.Errorf("expected status 'verified', got %q", q.statusSet)
		}
	})

	t.Run("onStatusChanged called when verification fails", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		// retryCount=4, maxRetries=5 → newRetryCount=5 >= 5 → mark failed
		articles := makeArticles(1, 4)
		q := &fakeQueue{articles: articles, countTotal: 1, countPend: 0, countFailed: 1}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(map[string]error{articles[0].MessageID: errors.New("not found")})).Times(1)

		called := false
		enabled := makeEnabled(true)
		cfg := config.PostCheck{
			Enabled:               enabled,
			DeferredCheckInterval: config.Duration("1m"),
			DeferredCheckDelay:    config.Duration("5m"),
			DeferredMaxBackoff:    config.Duration("1h"),
			DeferredMaxRetries:    5,
			DeferredBatchSize:     10,
		}
		w := NewPostCheckRetryWorker(context.Background(), q, mockPool, cfg, func() { called = true })

		w.processRetries()

		if !called {
			t.Error("expected onStatusChanged to be called after verification_failed, but it was not")
		}
		if q.statusSet != "verification_failed" {
			t.Errorf("expected status 'verification_failed', got %q", q.statusSet)
		}
	})

	t.Run("onStatusChanged called once per batch while articles still pending", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		articles := makeArticles(1, 0)                                   // retryCount=0, maxRetries=5 → schedules retry
		q := &fakeQueue{articles: articles, countTotal: 1, countPend: 1} // still pending
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(map[string]error{articles[0].MessageID: errors.New("not found")})).Times(1)

		calls := 0
		w := newWorker(context.Background(), q, mockPool, 10, 5)
		w.onStatusChanged = func() { calls++ }

		w.processRetries()

		// Batch-level notification keeps per-item verification progress fresh
		// in the UI even though no item reached a final status.
		if calls != 1 {
			t.Errorf("expected onStatusChanged to be called once per processed batch, got %d calls", calls)
		}
		if q.statusSetCount != 0 {
			t.Errorf("expected UpdateCompletedItemVerificationStatus not to be called, got %d calls", q.statusSetCount)
		}
	})

	t.Run("onStatusChanged still called once per batch when status update fails", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		articles := makeArticles(1, 0)
		q := &fakeQueue{
			articles:    articles,
			countTotal:  1,
			countPend:   0,
			countFailed: 0,
			statusErr:   errors.New("db error"),
		}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(nil)).Times(1)

		calls := 0
		w := newWorker(context.Background(), q, mockPool, 10, 5)
		w.onStatusChanged = func() { calls++ }

		w.processRetries()

		// The per-item notification is skipped on failure, but the batch-level
		// notification still fires so UI progress stays fresh.
		if calls != 1 {
			t.Errorf("expected onStatusChanged to be called once per processed batch, got %d calls", calls)
		}
	})

	t.Run("nil onStatusChanged does not panic", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		articles := makeArticles(1, 0)
		q := &fakeQueue{articles: articles, countTotal: 1, countPend: 0}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(statManyStub(nil)).Times(1)

		// nil callback — should not panic
		w := newWorker(context.Background(), q, mockPool, 10, 5)

		w.processRetries() // no panic expected
	})

	t.Run("bad groups JSON marks failed", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		articles := []queue.PendingArticleCheck{
			{
				ID:              99,
				CompletedItemID: "item-bad",
				MessageID:       "<bad@test>",
				Groups:          `not-valid-json`,
				Status:          "pending",
				RetryCount:      0,
			},
		}
		q := &fakeQueue{articles: articles, countTotal: 1, countPend: 0, countFailed: 1}
		mockPool := mocks.NewMockNNTPClient(ctrl)
		// Stat should NOT be called since JSON parsing fails first
		w := newWorker(context.Background(), q, mockPool, 10, 5)

		w.processRetries()

		if len(q.failed) != 1 || q.failed[0] != 99 {
			t.Errorf("expected article 99 to be marked failed due to bad JSON, got failed=%v", q.failed)
		}
		if len(q.verified) != 0 {
			t.Errorf("expected no verified marks, got %v", q.verified)
		}
	})
}

// statManyStub builds a StatMany stub that reports each id present unless it
// has an error in errs.
func statManyStub(errs map[string]error) func(context.Context, []string, nntppool.StatManyOptions) <-chan nntppool.StatManyResult {
	return func(_ context.Context, ids []string, _ nntppool.StatManyOptions) <-chan nntppool.StatManyResult {
		out := make(chan nntppool.StatManyResult, len(ids))
		for _, id := range ids {
			res := nntppool.StatManyResult{MessageID: id, Result: &nntppool.StatResult{MessageID: id}}
			if err, ok := errs[id]; ok {
				res = nntppool.StatManyResult{MessageID: id, Err: err}
			}
			out <- res
		}
		close(out)
		return out
	}
}
