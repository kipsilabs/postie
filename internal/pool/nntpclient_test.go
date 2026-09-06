package pool_test

import (
	"context"
	"errors"
	"testing"

	"github.com/javi11/nntppool/v4"
	"go.uber.org/mock/gomock"

	"github.com/kipsilabs/postie/internal/mocks"
	"github.com/kipsilabs/postie/internal/pool"
)

// statManyStub answers each id as present unless it has an error in errs. It
// also records the chunk sizes it was called with via calls.
func statManyStub(errs map[string]error, calls *[][]string) func(context.Context, []string, nntppool.StatManyOptions) <-chan nntppool.StatManyResult {
	return func(_ context.Context, ids []string, _ nntppool.StatManyOptions) <-chan nntppool.StatManyResult {
		if calls != nil {
			*calls = append(*calls, ids)
		}
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

func TestStatMissing_EmptyInput(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()
	client := mocks.NewMockNNTPClient(ctrl) // no StatMany expected

	missing, err := pool.StatMissing(context.Background(), client, nil, 100)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(missing) != 0 {
		t.Errorf("expected no missing ids, got %v", missing)
	}
}

func TestStatMissing_MissesReported(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()
	client := mocks.NewMockNNTPClient(ctrl)
	errs := map[string]error{
		"m1": nntppool.ErrArticleNotFound,
		"m3": errors.New("connection died"),
	}
	client.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).
		DoAndReturn(statManyStub(errs, nil)).Times(1)

	missing, err := pool.StatMissing(context.Background(), client, []string{"m0", "m1", "m2", "m3"}, 100)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	// Both a genuine 430 miss and a generic error count as missing.
	for _, id := range []string{"m1", "m3"} {
		if _, ok := missing[id]; !ok {
			t.Errorf("expected %s to be missing", id)
		}
	}
	for _, id := range []string{"m0", "m2"} {
		if _, ok := missing[id]; ok {
			t.Errorf("expected %s to be present", id)
		}
	}
}

func TestStatMissing_ChunksByBatchSize(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()
	client := mocks.NewMockNNTPClient(ctrl)

	var calls [][]string
	client.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).
		DoAndReturn(statManyStub(nil, &calls)).Times(3)

	ids := []string{"a", "b", "c", "d", "e"}
	if _, err := pool.StatMissing(context.Background(), client, ids, 2); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	want := [][]int{{2}, {2}, {1}}
	if len(calls) != 3 || len(calls[0]) != 2 || len(calls[1]) != 2 || len(calls[2]) != 1 {
		t.Errorf("expected chunk sizes 2,2,1 (%v), got %v", want, calls)
	}
}

func TestStatMissing_UnreportedIDsCountAsMissing(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()
	client := mocks.NewMockNNTPClient(ctrl)

	// Simulate an interrupted sweep that drops the second id entirely.
	client.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).DoAndReturn(
		func(_ context.Context, ids []string, _ nntppool.StatManyOptions) <-chan nntppool.StatManyResult {
			out := make(chan nntppool.StatManyResult, 1)
			out <- nntppool.StatManyResult{MessageID: ids[0], Result: &nntppool.StatResult{MessageID: ids[0]}}
			close(out)
			return out
		}).Times(1)

	missing, err := pool.StatMissing(context.Background(), client, []string{"a", "b"}, 100)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if _, ok := missing["b"]; !ok {
		t.Error("expected unreported id b to count as missing")
	}
	if _, ok := missing["a"]; ok {
		t.Error("expected reported id a to be present")
	}
}

func TestStatMissing_CancelledContextReturnsError(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()
	client := mocks.NewMockNNTPClient(ctrl)
	client.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).
		DoAndReturn(statManyStub(nil, nil)).AnyTimes()

	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	if _, err := pool.StatMissing(ctx, client, []string{"a"}, 100); !errors.Is(err, context.Canceled) {
		t.Errorf("expected context.Canceled, got %v", err)
	}
}

func TestStatMissing_ZeroBatchSizeUsesDefault(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()
	client := mocks.NewMockNNTPClient(ctrl)

	var calls [][]string
	client.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).
		DoAndReturn(statManyStub(nil, &calls)).Times(2)

	ids := make([]string, pool.DefaultStatBatchSize+1)
	for i := range ids {
		ids[i] = string(rune('a' + i%26))
	}
	if _, err := pool.StatMissing(context.Background(), client, ids, 0); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(calls) != 2 || len(calls[0]) != pool.DefaultStatBatchSize || len(calls[1]) != 1 {
		t.Errorf("expected chunks of %d and 1, got sizes %d", pool.DefaultStatBatchSize, len(calls))
	}
}
