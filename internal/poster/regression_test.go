package poster

import (
	"context"
	"os"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/javi11/nntppool/v4"
	"github.com/kipsilabs/postie/internal/article"
	"github.com/kipsilabs/postie/internal/mocks"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/mock/gomock"
)

// waitGroupDrained fails the test if wg.Wait() does not return within timeout.
func waitGroupDrained(t *testing.T, wg *sync.WaitGroup, timeout time.Duration, msg string) {
	t.Helper()
	done := make(chan struct{})
	go func() {
		wg.Wait()
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(timeout):
		t.Fatal(msg)
	}
}

// newRegressionMocks builds the standard mock set used by the regression tests.
func newRegressionMocks(t *testing.T) (*gomock.Controller, *mocks.MockNNTPClient, *mocks.MockNZBGenerator, *mocks.MockJobProgress, *mocks.MockProgress) {
	t.Helper()
	ctrl := gomock.NewController(t)

	mockJobProgress := mocks.NewMockJobProgress(ctrl)
	mockProgress := mocks.NewMockProgress(ctrl)
	mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
	mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
	mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
	mockProgress.EXPECT().Finish().AnyTimes()
	mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()
	mockProgress.EXPECT().SetWaitDeadline(gomock.Any()).AnyTimes()

	mockPool := mocks.NewMockNNTPClient(ctrl)
	mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
		Providers: []nntppool.ProviderStats{{MaxConnections: 4}},
	}).AnyTimes()

	nzbGen := mocks.NewMockNZBGenerator(ctrl)
	nzbGen.EXPECT().AddArticle(gomock.Any()).Return().AnyTimes()

	return ctrl, mockPool, nzbGen, mockJobProgress, mockProgress
}

func newRegressionPoster(mockPool *mocks.MockNNTPClient, mockJobProgress *mocks.MockJobProgress) *poster {
	checkCfg := createTestPostCheckConfig()
	enabled := false
	checkCfg.Enabled = &enabled
	return &poster{
		cfg:         createTestConfig(),
		checkCfg:    checkCfg,
		uploadPool:  mockPool,
		stats:       &Stats{StartTime: time.Now()},
		throttle:    NewThrottle(1024*1024, time.Second),
		jobProgress: mockJobProgress,
	}
}

// Regression: a zero-byte input file used to leave the per-file WaitGroup
// unbalanced (wg.Add up front, no wg.Done on the skip path), hanging Post()
// forever.
func TestPost_EmptyFileDoesNotHang(t *testing.T) {
	ctx := context.Background()
	emptyFile := createTestFile(t, "")
	defer func() { _ = os.Remove(emptyFile) }()

	ctrl, mockPool, nzbGen, mockJobProgress, _ := newRegressionMocks(t)
	defer ctrl.Finish()
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil).AnyTimes()

	p := newRegressionPoster(mockPool, mockJobProgress)
	defer p.Close()

	errCh := make(chan error, 1)
	go func() { errCh <- p.Post(ctx, []string{emptyFile}, "", nzbGen) }()

	select {
	case err := <-errCh:
		assert.NoError(t, err)
	case <-time.After(10 * time.Second):
		t.Fatal("Post hung on an empty input file")
	}
}

// Regression: postYenc mapped its internal per-article deadline to
// context.Canceled even when the caller's context was still alive, which made
// postLoop skip error reporting and hang Post() forever.
func TestPostYenc_InternalTimeoutSurfacesRealError(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	mockPool := mocks.NewMockNNTPClient(ctrl)
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(nil, context.DeadlineExceeded).Times(1)

	art := &article.Article{MessageID: "<x@test>", Groups: []string{"alt.test"}, Size: 4}
	err := postYenc(context.Background(), mockPool, nil, nil, art, []byte("body"))

	require.Error(t, err)
	assert.NotErrorIs(t, err, context.Canceled)
	assert.ErrorIs(t, err, context.DeadlineExceeded)
}

func TestPostYenc_ParentCancellationIsCanceled(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	mockPool := mocks.NewMockNNTPClient(ctrl)
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(nil, context.Canceled).AnyTimes()

	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	art := &article.Article{MessageID: "<x@test>", Groups: []string{"alt.test"}, Size: 4}
	err := postYenc(ctx, mockPool, nil, nil, art, []byte("body"))

	assert.ErrorIs(t, err, context.Canceled)
}

// Regression: an article post that times out internally (parent ctx alive)
// must surface as a Post() error instead of hanging forever.
func TestPost_ArticleTimeoutReturnsError(t *testing.T) {
	ctx := context.Background()
	testFile := createTestFile(t, "some content")
	defer func() { _ = os.Remove(testFile) }()

	ctrl, mockPool, nzbGen, mockJobProgress, _ := newRegressionMocks(t)
	defer ctrl.Finish()
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(nil, context.DeadlineExceeded).AnyTimes()

	p := newRegressionPoster(mockPool, mockJobProgress)
	defer p.Close()

	errCh := make(chan error, 1)
	go func() { errCh <- p.Post(ctx, []string{testFile}, "", nzbGen) }()

	select {
	case err := <-errCh:
		require.Error(t, err)
		assert.Contains(t, err.Error(), "failed to post file")
	case <-time.After(10 * time.Second):
		t.Fatal("Post hung on an internal article post timeout")
	}
}

// Regression: a read-ahead failure mid-file used to silently drop the
// remaining segments while reporting the post as fully posted, truncating the
// uploaded article set and the NZB.
func TestPostLoop_ReadErrorFailsPost(t *testing.T) {
	ctx := context.Background()
	testFile := createTestFile(t, "short")
	defer func() { _ = os.Remove(testFile) }()

	ctrl, mockPool, nzbGen, mockJobProgress, mockProgress := newRegressionMocks(t)
	defer ctrl.Finish()
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil).AnyTimes()

	p := newRegressionPoster(mockPool, mockJobProgress)
	p.ensureWorkersStarted()
	defer p.Close()

	f, err := os.Open(testFile)
	require.NoError(t, err)

	postQueue := make(chan *Post, 2)
	checkQueue := make(chan *Post, 2)
	errChan := make(chan error, 4)
	var postsInFlight sync.WaitGroup
	var wg sync.WaitGroup
	wg.Add(1)

	post := &Post{
		FilePath: testFile,
		// Offset far beyond EOF forces a deterministic ReadAt error.
		Articles: []*article.Article{{MessageID: "<x@test>", Groups: []string{"alt.test"}, Size: 100, Offset: 1 << 20}},
		Status:   PostStatusPending,
		file:     f,
		filesize: 5,
		wg:       &wg,
		progress: mockProgress,
	}
	postsInFlight.Add(1)
	postQueue <- post
	go func() {
		postsInFlight.Wait()
		close(postQueue)
	}()

	go p.postLoop(ctx, postQueue, checkQueue, errChan, nzbGen, &postsInFlight)

	select {
	case err := <-errChan:
		require.Error(t, err)
		assert.Contains(t, err.Error(), "pre-reading")
	case <-time.After(10 * time.Second):
		t.Fatal("postLoop reported success despite a read-ahead failure")
	}
	waitGroupDrained(t, &wg, 5*time.Second, "per-file WaitGroup leaked after read failure")
}

// Regression: when postLoop exited early on a fatal error, posts still queued
// behind the failing one leaked their file descriptors and WaitGroup counts.
func TestPostLoop_FailureDrainsQueuedPosts(t *testing.T) {
	ctx := context.Background()
	failFile := createTestFile(t, "fail")
	okFile := createTestFile(t, "ok")
	defer func() { _ = os.Remove(failFile); _ = os.Remove(okFile) }()

	ctrl, mockPool, nzbGen, mockJobProgress, mockProgress := newRegressionMocks(t)
	defer ctrl.Finish()
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil).AnyTimes()

	p := newRegressionPoster(mockPool, mockJobProgress)
	p.ensureWorkersStarted()
	defer p.Close()

	f1, err := os.Open(failFile)
	require.NoError(t, err)
	f2, err := os.Open(okFile)
	require.NoError(t, err)

	postQueue := make(chan *Post, 2)
	checkQueue := make(chan *Post, 2)
	errChan := make(chan error, 4)
	var postsInFlight sync.WaitGroup
	var wg sync.WaitGroup
	wg.Add(2)

	post1 := &Post{
		FilePath: failFile,
		Articles: []*article.Article{{MessageID: "<a@test>", Groups: []string{"alt.test"}, Size: 100, Offset: 1 << 20}},
		Status:   PostStatusPending,
		file:     f1,
		filesize: 4,
		wg:       &wg,
		progress: mockProgress,
	}
	post2 := &Post{
		FilePath: okFile,
		Articles: []*article.Article{{MessageID: "<b@test>", Groups: []string{"alt.test"}, Size: 2, Offset: 0}},
		Status:   PostStatusPending,
		file:     f2,
		filesize: 2,
		wg:       &wg,
		progress: mockProgress,
	}
	postsInFlight.Add(2)
	postQueue <- post1
	postQueue <- post2
	go func() {
		postsInFlight.Wait()
		close(postQueue)
	}()

	go p.postLoop(ctx, postQueue, checkQueue, errChan, nzbGen, &postsInFlight)

	select {
	case err := <-errChan:
		require.Error(t, err)
	case <-time.After(10 * time.Second):
		t.Fatal("postLoop did not report the failure")
	}

	// Both posts' WaitGroup counts must drain (post2 via the abandon drain).
	waitGroupDrained(t, &wg, 5*time.Second, "queued post leaked its WaitGroup count after early loop exit")

	// The queued post's file descriptor must be closed by the drain.
	assert.Eventually(t, func() bool {
		buf := make([]byte, 1)
		_, readErr := f2.Read(buf)
		return readErr != nil && readErr.Error() != ""
	}, 5*time.Second, 10*time.Millisecond, "queued post's file was not closed by the drain")
	buf := make([]byte, 1)
	_, readErr := f2.Read(buf)
	assert.ErrorIs(t, readErr, os.ErrClosed)
}

// Regression: poster and reposter used to each build a private token bucket,
// so the configured throttle rate bounded each instance instead of aggregate
// egress. Everything sharing an engine must share one throttle.
func TestSharedThrottle_OneBucketPerEngine(t *testing.T) {
	e := NewEngine(750_000, 0, 4)

	t1 := e.SharedThrottle(1024)
	t2 := e.SharedThrottle(1024)
	require.NotNil(t, t1)
	assert.Same(t, t1, t2, "same engine must return the same throttle instance")

	assert.Nil(t, e.SharedThrottle(0), "rate <= 0 disables throttling")

	var nilEngine *Engine
	standalone := nilEngine.SharedThrottle(1024)
	require.NotNil(t, standalone, "nil engine falls back to a private throttle")
	assert.NotSame(t, t1, standalone)
}
