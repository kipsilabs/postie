package poster

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/javi11/nntppool/v4"
	"go.uber.org/mock/gomock"

	"github.com/javi11/postie/internal/article"
	"github.com/javi11/postie/internal/manifest"
	"github.com/javi11/postie/internal/mocks"
)

// existingManifestSink reports a pre-recorded manifest for every file so the
// poster takes the recovery path (retry after failure or restart).
type existingManifestSink struct {
	recs []manifest.ArticleRecord
}

func (s *existingManifestSink) RecordFile(context.Context, string, []*article.Article) error {
	return nil
}

func (s *existingManifestSink) ExistingArticles(context.Context, string) ([]manifest.ArticleRecord, bool, error) {
	return s.recs, len(s.recs) > 0, nil
}

func recoveryRecords(filePath string, content string) []manifest.ArticleRecord {
	half := int64(len(content) / 2)
	mk := func(i int, off, size int64) manifest.ArticleRecord {
		return manifest.ArticleRecord{
			Index: i, SourcePath: filePath, FileRole: manifest.RoleOriginal,
			Offset: off, BodySize: uint64(size), MessageID: "rec-" + string(rune('a'+i)),
			Subject: "subj", OriginalSubject: "osubj", From: "x@y", Groups: []string{"alt.test"},
			FileName: "obf", PartNumber: i + 1, TotalParts: 2, FileSize: int64(len(content)),
		}
	}
	return []manifest.ArticleRecord{mk(0, 0, half), mk(1, half, int64(len(content))-half)}
}

// runRecoveredPost posts testFile through the recovery path with the given set
// of message IDs reported missing by STAT, and returns the articles the NZB
// generator received.
func runRecoveredPost(t *testing.T, missing map[string]error) (string, []*article.Article, error) {
	t.Helper()
	content := strings.Repeat("recovered data ", 200)
	testFile := createTestFile(t, content)
	t.Cleanup(func() { _ = os.Remove(testFile) })

	ctrl := gomock.NewController(t)
	t.Cleanup(ctrl.Finish)

	mockPool := mocks.NewMockNNTPClient(ctrl)
	mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
		Providers: []nntppool.ProviderStats{{MaxConnections: 2}},
	}).AnyTimes()
	mockPool.EXPECT().StatMany(gomock.Any(), gomock.Any(), gomock.Any()).
		DoAndReturn(statManyStub(missing)).AnyTimes()
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).
		Return(&nntppool.PostResult{}, nil).Times(len(missing))

	var mu sync.Mutex
	var added []*article.Article
	nzbGen := mocks.NewMockNZBGenerator(ctrl)
	nzbGen.EXPECT().AddArticle(gomock.Any()).DoAndReturn(func(a *article.Article) {
		mu.Lock()
		defer mu.Unlock()
		added = append(added, a)
	}).AnyTimes()

	mockJobProgress := mocks.NewMockJobProgress(ctrl)
	mockProgress := mocks.NewMockProgress(ctrl)
	mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
	mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
	mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
	mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

	checkCfg := createTestPostCheckConfig()
	disabled := false
	checkCfg.Enabled = &disabled

	p := &poster{
		cfg:          createTestConfig(),
		checkCfg:     checkCfg,
		uploadPool:   mockPool,
		verifyPool:   mockPool,
		stats:        &Stats{StartTime: time.Now()},
		jobProgress:  mockJobProgress,
		manifestSink: &existingManifestSink{recs: recoveryRecords(testFile, content)},
	}
	err := p.Post(context.Background(), []string{testFile}, "", nzbGen)
	p.Close()

	mu.Lock()
	defer mu.Unlock()
	return testFile, added, err
}

func assertNZBHasBothSegments(t *testing.T, testFile string, added []*article.Article) {
	t.Helper()
	if len(added) != 2 {
		t.Fatalf("NZB received %d articles, want 2 (every planned segment, posted or already present)", len(added))
	}
	ids := map[string]bool{}
	for _, a := range added {
		ids[a.MessageID] = true
		if a.OriginalName != filepath.Base(testFile) {
			t.Errorf("article %s OriginalName=%q, want %q (NZB groups segments by it)", a.MessageID, a.OriginalName, filepath.Base(testFile))
		}
		if a.FileNumber != 1 {
			t.Errorf("article %s FileNumber=%d, want 1", a.MessageID, a.FileNumber)
		}
	}
	if !ids["rec-a"] || !ids["rec-b"] {
		t.Errorf("NZB is missing segments: got %v", ids)
	}
}

// TestRecoveredPost_AllPresentStillFillsNZB: after a retry or restart every
// article may already be on the server. Nothing needs re-posting, but the NZB
// must still list all segments or the output is unusable.
func TestRecoveredPost_AllPresentStillFillsNZB(t *testing.T) {
	testFile, added, err := runRecoveredPost(t, nil)
	if err != nil {
		t.Fatalf("Post: %v", err)
	}
	assertNZBHasBothSegments(t, testFile, added)
}

// TestRecoveredPost_PartiallyPresentFillsNZB: only the missing article is
// re-posted, but the NZB must contain both the re-posted and the already
// present segment.
func TestRecoveredPost_PartiallyPresentFillsNZB(t *testing.T) {
	testFile, added, err := runRecoveredPost(t, map[string]error{"rec-b": errors.New("430 no such article")})
	if err != nil {
		t.Fatalf("Post: %v", err)
	}
	assertNZBHasBothSegments(t, testFile, added)
}
