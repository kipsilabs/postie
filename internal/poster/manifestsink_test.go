package poster

import (
	"context"
	"errors"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/javi11/nntppool/v4"
	"go.uber.org/mock/gomock"

	"github.com/kipsilabs/postie/internal/article"
	"github.com/kipsilabs/postie/internal/manifest"
	"github.com/kipsilabs/postie/internal/mocks"
)

// recordingSink captures RecordFile calls so tests can assert the poster writes
// a manifest before posting.
type recordingSink struct {
	mu    sync.Mutex
	calls []sinkCall
	err   error
}

type sinkCall struct {
	path     string
	articles int
}

func (s *recordingSink) RecordFile(_ context.Context, filePath string, articles []*article.Article) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.calls = append(s.calls, sinkCall{path: filePath, articles: len(articles)})
	return s.err
}

// ExistingArticles always reports "no manifest" so these tests exercise the
// fresh-upload path (recovery is covered in the transferwriter package).
func (s *recordingSink) ExistingArticles(_ context.Context, _ string) ([]manifest.ArticleRecord, bool, error) {
	return nil, false, nil
}

func TestPost_RecordsManifestBeforePosting(t *testing.T) {
	ctx := context.Background()
	content := strings.Repeat("test data ", 100)
	testFile := createTestFile(t, content)
	defer func() { _ = os.Remove(testFile) }()

	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	mockPool := mocks.NewMockNNTPClient(ctrl)
	mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
		Providers: []nntppool.ProviderStats{{MaxConnections: 4}},
	}).AnyTimes()
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).
		Return(&nntppool.PostResult{}, nil).AnyTimes()

	nzbGen := mocks.NewMockNZBGenerator(ctrl)
	nzbGen.EXPECT().AddArticle(gomock.Any()).Return().AnyTimes()

	mockJobProgress := mocks.NewMockJobProgress(ctrl)
	mockProgress := mocks.NewMockProgress(ctrl)
	mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
	mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
	mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
	mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

	checkCfg := createTestPostCheckConfig()
	disabled := false
	checkCfg.Enabled = &disabled

	sink := &recordingSink{}
	p := &poster{
		cfg:          createTestConfig(),
		checkCfg:     checkCfg,
		uploadPool:   mockPool,
		stats:        &Stats{StartTime: time.Now()},
		jobProgress:  mockJobProgress,
		manifestSink: sink,
	}

	if err := p.Post(ctx, []string{testFile}, "", nzbGen); err != nil {
		t.Fatalf("Post: %v", err)
	}
	p.Close()

	if len(sink.calls) != 1 {
		t.Fatalf("RecordFile called %d times, want 1", len(sink.calls))
	}
	if sink.calls[0].path != testFile {
		t.Errorf("RecordFile path = %q, want %q", sink.calls[0].path, testFile)
	}
	if sink.calls[0].articles == 0 {
		t.Errorf("RecordFile received 0 articles, want > 0")
	}
}

func TestPost_SinkErrorAbortsBeforePosting(t *testing.T) {
	ctx := context.Background()
	testFile := createTestFile(t, strings.Repeat("x", 2000))
	defer func() { _ = os.Remove(testFile) }()

	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	mockPool := mocks.NewMockNNTPClient(ctrl)
	mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
		Providers: []nntppool.ProviderStats{{MaxConnections: 4}},
	}).AnyTimes()
	// PostYenc must NEVER be called when the manifest cannot be written.
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).
		Times(0)

	nzbGen := mocks.NewMockNZBGenerator(ctrl)
	nzbGen.EXPECT().AddArticle(gomock.Any()).AnyTimes()
	mockJobProgress := mocks.NewMockJobProgress(ctrl)
	mockProgress := mocks.NewMockProgress(ctrl)
	mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
	mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
	mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

	checkCfg := createTestPostCheckConfig()
	disabled := false
	checkCfg.Enabled = &disabled

	sink := &recordingSink{err: errors.New("disk full")}
	p := &poster{
		cfg:          createTestConfig(),
		checkCfg:     checkCfg,
		uploadPool:   mockPool,
		stats:        &Stats{StartTime: time.Now()},
		jobProgress:  mockJobProgress,
		manifestSink: sink,
	}

	if err := p.Post(ctx, []string{testFile}, "", nzbGen); err == nil {
		t.Fatal("Post should fail when the manifest sink errors")
	}
	p.Close()
}
