package poster

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/javi11/nntppool/v4"
	"github.com/kipsilabs/postie/internal/article"
	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/mocks"
	"github.com/kipsilabs/postie/internal/pausable"
	"github.com/kipsilabs/postie/internal/pool"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/mock/gomock"
)

func TestNew(t *testing.T) {
	t.Run("success", func(t *testing.T) {
		ctx := context.Background()
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := createMockNNTPClient(ctrl)
		mockPoolManager := createMockPoolManager(ctrl, mockPool)

		mockConfig := mocks.NewMockConfig(ctrl)
		mockConfig.EXPECT().GetPostingConfig().Return(createTestConfig())
		mockConfig.EXPECT().GetPostCheckConfig().Return(createTestPostCheckConfig())

		mockJobProgress := mocks.NewMockJobProgress(ctrl)

		poster, err := New(ctx, mockConfig, mockPoolManager, mockJobProgress)

		require.NoError(t, err)
		assert.NotNil(t, poster)
	})

	t.Run("nil pool manager error", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		ctx := context.Background()

		mockConfig := mocks.NewMockConfig(ctrl)
		mockJobProgress := mocks.NewMockJobProgress(ctrl)

		poster, err := New(ctx, mockConfig, nil, mockJobProgress)

		assert.Error(t, err)
		assert.Nil(t, poster)
		assert.Contains(t, err.Error(), "pool manager cannot be nil")
	})

	t.Run("success with pausable context", func(t *testing.T) {
		ctx := pausable.NewContext(context.Background())
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := createMockNNTPClient(ctrl)
		mockPoolManager := createMockPoolManager(ctrl, mockPool)

		mockConfig := mocks.NewMockConfig(ctrl)
		mockConfig.EXPECT().GetPostingConfig().Return(createTestConfig())
		mockConfig.EXPECT().GetPostCheckConfig().Return(createTestPostCheckConfig())

		mockJobProgress := mocks.NewMockJobProgress(ctrl)

		poster, err := New(ctx, mockConfig, mockPoolManager, mockJobProgress)

		require.NoError(t, err)
		assert.NotNil(t, poster)
	})

	t.Run("pool unavailable error", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		ctx := context.Background()

		// Create a pool manager that returns nil pool
		mockPoolManager := createMockPoolManager(ctrl, nil)

		mockConfig := mocks.NewMockConfig(ctrl)
		mockJobProgress := mocks.NewMockJobProgress(ctrl)

		poster, err := New(ctx, mockConfig, mockPoolManager, mockJobProgress)

		assert.Error(t, err)
		assert.Nil(t, poster)
		assert.Contains(t, err.Error(), "connection pool is not available")
	})
}

func TestPost(t *testing.T) {
	t.Run("success with single file", func(t *testing.T) {
		ctx := context.Background()
		content := strings.Repeat("test data ", 100) // Create content larger than segment size
		testFile := createTestFile(t, content)
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		ctrl := gomock.NewController(t)

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil).AnyTimes()

		nzbGen := mocks.NewMockNZBGenerator(ctrl)
		nzbGen.EXPECT().AddArticle(gomock.Any()).Return().AnyTimes()

		// Mock the job progress
		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		// Create poster with check disabled to simplify test
		checkCfg := createTestPostCheckConfig()
		enabled := false
		checkCfg.Enabled = &enabled

		p := &poster{
			cfg:         createTestConfig(),
			checkCfg:    checkCfg,
			uploadPool:  mockPool,
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mockJobProgress,
		}

		err := p.Post(ctx, []string{testFile}, "", nzbGen)

		assert.NoError(t, err)

		// Close after test completes
		p.Close()

		// Finish controller after all operations complete
		ctrl.Finish()
	})

	t.Run("success with multiple files", func(t *testing.T) {
		ctx := context.Background()

		// Create test files
		testFile1 := createTestFile(t, "test content 1")
		testFile2 := createTestFile(t, "test content 2")
		defer func() {
			err := os.Remove(testFile1)
			assert.NoError(t, err, "Failed to remove test file")
		}()
		defer func() {
			err := os.Remove(testFile2)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		ctrl := gomock.NewController(t)

		// Mock the job progress
		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil).AnyTimes()

		nzbGen := mocks.NewMockNZBGenerator(ctrl)
		nzbGen.EXPECT().AddArticle(gomock.Any()).Return().AnyTimes()

		// Create poster with check disabled to simplify test
		checkCfg := createTestPostCheckConfig()
		enabled := false
		checkCfg.Enabled = &enabled

		p := &poster{
			cfg:         createTestConfig(),
			checkCfg:    checkCfg,
			uploadPool:  mockPool,
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mockJobProgress,
		}

		err := p.Post(ctx, []string{testFile1, testFile2}, "", nzbGen)

		assert.NoError(t, err)

		// Close after test completes
		p.Close()

		// Finish controller after all operations complete
		ctrl.Finish()
	})

	t.Run("posting failure", func(t *testing.T) {
		ctx := context.Background()
		testFile := createTestFile(t, "test content")
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		ctrl := gomock.NewController(t)

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(nil, errors.New("posting failed")).AnyTimes()

		nzbGen := mocks.NewMockNZBGenerator(ctrl)
		nzbGen.EXPECT().AddArticle(gomock.Any()).Return().AnyTimes()

		// Mock the job progress
		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		// Create poster with check disabled to simplify test
		checkCfg := createTestPostCheckConfig()
		enabled := false
		checkCfg.Enabled = &enabled

		p := &poster{
			cfg:         createTestConfig(),
			checkCfg:    checkCfg,
			uploadPool:  mockPool,
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mockJobProgress,
		}

		err := p.Post(ctx, []string{testFile}, "", nzbGen)

		assert.Error(t, err)
		assert.Contains(t, err.Error(), "failed to post file")

		// Close after test completes
		p.Close()

		// Finish controller after all operations complete
		ctrl.Finish()
	})

	t.Run("check enabled success", func(t *testing.T) {
		ctx := context.Background()
		testFile := createTestFile(t, "test content")
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		ctrl := gomock.NewController(t)

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil).AnyTimes()
		mockPool.EXPECT().Stat(gomock.Any(), gomock.Any()).Return(&nntppool.StatResult{}, nil).AnyTimes()

		nzbGen := mocks.NewMockNZBGenerator(ctrl)
		nzbGen.EXPECT().AddArticle(gomock.Any()).Return().AnyTimes()

		// Mock the job progress
		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		checkCfg := createTestPostCheckConfig()
		enabled := true
		checkCfg.Enabled = &enabled
		checkCfg.RetryDelay = config.Duration("0s") // avoid real sleep in tests

		p := &poster{
			cfg:         createTestConfig(),
			checkCfg:    checkCfg,
			uploadPool:  mockPool,
			verifyPool:  mockPool, // Use same pool for checking
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mockJobProgress,
		}

		err := p.Post(ctx, []string{testFile}, "", nzbGen)

		assert.NoError(t, err)

		// Close after test completes
		p.Close()

		// Finish controller after all operations complete
		ctrl.Finish()
	})

	t.Run("check enabled failure", func(t *testing.T) {
		ctx := context.Background()
		testFile := createTestFile(t, "test content")
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		ctrl := gomock.NewController(t)

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil).AnyTimes()
		mockPool.EXPECT().Stat(gomock.Any(), gomock.Any()).Return(nil, errors.New("article not found")).AnyTimes()

		nzbGen := mocks.NewMockNZBGenerator(ctrl)
		nzbGen.EXPECT().AddArticle(gomock.Any()).Return().AnyTimes()

		// Mock the job progress
		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		checkCfg := createTestPostCheckConfig()
		enabled := true
		checkCfg.Enabled = &enabled
		checkCfg.MaxRePost = 0                      // No retries
		checkCfg.RetryDelay = config.Duration("0s") // avoid real sleep in tests

		p := &poster{
			cfg:         createTestConfig(),
			checkCfg:    checkCfg,
			uploadPool:  mockPool,
			verifyPool:  mockPool, // Use same pool for checking
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mockJobProgress,
		}

		err := p.Post(ctx, []string{testFile}, "", nzbGen)

		assert.Error(t, err)
		assert.Contains(t, err.Error(), "failed to verify file")

		// Close after test completes
		p.Close()

		// Finish controller after all operations complete
		ctrl.Finish()
	})

	t.Run("article stat fails and gets reuploaded successfully", func(t *testing.T) {
		// Instead of a full integration test, test the checkArticle method directly
		// to verify retry behavior without triggering progress bar issues
		ctx := context.Background()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()

		// Test checkArticle method directly
		p := &poster{
			uploadPool: mockPool,
			verifyPool: mockPool, // Use same pool for checking
			stats:      &Stats{StartTime: time.Now()},
		}
		defer p.Close()

		art := &article.Article{
			MessageID: "test@example.com",
			Groups:    []string{"alt.test"},
		}

		// First call fails (article not found)
		mockPool.EXPECT().Stat(ctx, "test@example.com").Return(nil, errors.New("article not found")).Times(1)

		err := p.checkArticle(ctx, art)
		assert.Error(t, err)
		assert.Contains(t, err.Error(), "article not found")
		assert.Equal(t, int64(0), p.stats.ArticlesChecked) // Should not increment on failure

		// Second call succeeds (article found after retry)
		mockPool.EXPECT().Stat(ctx, "test@example.com").Return(&nntppool.StatResult{}, nil).Times(1)

		err = p.checkArticle(ctx, art)
		assert.NoError(t, err)
		assert.Equal(t, int64(1), p.stats.ArticlesChecked) // Should increment on success
	})

	t.Run("article stat fails repeatedly and exceeds max retries", func(t *testing.T) {
		// Test the retry limit behavior using checkArticle method
		ctx := context.Background()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()

		p := &poster{
			uploadPool: mockPool,
			verifyPool: mockPool, // Use same pool for checking
			stats:      &Stats{StartTime: time.Now()},
		}
		defer p.Close()

		art := &article.Article{
			MessageID: "test@example.com",
			Groups:    []string{"alt.test"},
		}

		// Simulate multiple failed attempts
		for range 3 {
			mockPool.EXPECT().Stat(ctx, "test@example.com").Return(nil, errors.New("article not found")).Times(1)

			err := p.checkArticle(ctx, art)
			assert.Error(t, err)
			assert.Contains(t, err.Error(), "article not found")
		}

		// Stats should still be 0 since all calls failed
		assert.Equal(t, int64(0), p.stats.ArticlesChecked)
	})

	t.Run("postArticle and checkArticle integration", func(t *testing.T) {
		// Test posting and checking an article without the full Post workflow
		ctx := context.Background()
		content := "test article content"
		testFile := createTestFile(t, content)
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		file, err := os.Open(testFile)
		require.NoError(t, err)
		defer func() {
			if err := file.Close(); err != nil && !strings.Contains(err.Error(), "file already closed") {
				assert.NoError(t, err, "Failed to close test file")
			}
		}()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()

		// Post should succeed
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil)
		// Check should fail first time
		mockPool.EXPECT().Stat(gomock.Any(), gomock.Any()).Return(nil, errors.New("not found"))
		// Check should succeed second time
		mockPool.EXPECT().Stat(gomock.Any(), gomock.Any()).Return(&nntppool.StatResult{}, nil)

		p := &poster{
			uploadPool:  mockPool,
			verifyPool:  mockPool, // Use same pool for checking
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mocks.NewMockJobProgress(ctrl),
		}
		defer p.Close()

		art := &article.Article{
			MessageID:  "test@example.com",
			Subject:    "Test Subject",
			From:       "test@example.com",
			Groups:     []string{"alt.test"},
			PartNumber: 1,
			TotalParts: 1,
			FileName:   "test.txt",
			Date:       time.Now(),
			Offset:     0,
			Size:       uint64(len(content)),
			FileSize:   int64(len(content)),
		}

		// Post the article
		err = p.postArticle(ctx, art, file)
		assert.NoError(t, err)
		assert.Equal(t, int64(1), p.stats.ArticlesPosted)

		// Check fails first time (simulating article not propagated yet)
		err = p.checkArticle(ctx, art)
		assert.Error(t, err)
		assert.Equal(t, int64(0), p.stats.ArticlesChecked)

		// Check succeeds second time (simulating article now available)
		err = p.checkArticle(ctx, art)
		assert.NoError(t, err)
		assert.Equal(t, int64(1), p.stats.ArticlesChecked)
	})

	t.Run("file not found", func(t *testing.T) {
		ctx := context.Background()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		nzbGen := mocks.NewMockNZBGenerator(ctrl)
		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()

		p := &poster{
			cfg:         createTestConfig(),
			checkCfg:    createTestPostCheckConfig(),
			uploadPool:  mockPool,
			verifyPool:  mockPool, // Use same pool for checking
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mocks.NewMockJobProgress(ctrl),
		}
		defer p.Close()

		err := p.Post(ctx, []string{"nonexistent.txt"}, "", nzbGen)

		assert.Error(t, err)
		assert.Contains(t, err.Error(), "error adding file")
	})
}

func TestStats(t *testing.T) {
	startTime := time.Now()
	p := &poster{
		stats: &Stats{
			ArticlesPosted:  10,
			ArticlesChecked: 8,
			BytesPosted:     1024,
			ArticleErrors:   2,
			StartTime:       startTime,
		},
	}

	stats := p.Stats()

	assert.Equal(t, int64(10), stats.ArticlesPosted)
	assert.Equal(t, int64(8), stats.ArticlesChecked)
	assert.Equal(t, int64(1024), stats.BytesPosted)
	assert.Equal(t, int64(2), stats.ArticleErrors)
	assert.Equal(t, startTime, stats.StartTime)
}

func TestPostArticle(t *testing.T) {
	t.Run("success", func(t *testing.T) {
		ctx := context.Background()
		content := "test article content"
		testFile := createTestFile(t, content)
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		// Open the file
		file, err := os.Open(testFile)
		require.NoError(t, err)
		defer func() {
			if err := file.Close(); err != nil && !strings.Contains(err.Error(), "file already closed") {
				assert.NoError(t, err, "Failed to close test file")
			}
		}()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil)

		p := &poster{
			uploadPool:  mockPool,
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mocks.NewMockJobProgress(ctrl),
		}

		art := &article.Article{
			MessageID:  "test@example.com",
			Subject:    "Test Subject",
			From:       "test@example.com",
			Groups:     []string{"alt.test"},
			PartNumber: 1,
			TotalParts: 1,
			FileName:   "test.txt",
			Date:       time.Now(),
			Offset:     0,
			Size:       uint64(len(content)),
			FileSize:   int64(len(content)),
		}

		err = p.postArticle(ctx, art, file)

		assert.NoError(t, err)
		assert.Equal(t, int64(1), p.stats.ArticlesPosted)
		assert.Equal(t, int64(len(content)), p.stats.BytesPosted)
	})

	t.Run("file read error", func(t *testing.T) {
		ctx := context.Background()

		// Create a file and then close it to simulate read error
		testFile := createTestFile(t, "test content")
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		file, err := os.Open(testFile)
		require.NoError(t, err)
		_ = file.Close() // Close the file to cause read error

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()

		p := &poster{
			uploadPool:  mockPool,
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mocks.NewMockJobProgress(ctrl),
		}

		art := &article.Article{
			Offset: 0,
			Size:   10,
		}

		err = p.postArticle(ctx, art, file)

		assert.Error(t, err)
		assert.Contains(t, err.Error(), "error reading article body")
	})

	t.Run("post error", func(t *testing.T) {
		ctx := context.Background()
		content := "test content"
		testFile := createTestFile(t, content)
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		file, err := os.Open(testFile)
		require.NoError(t, err)
		defer func() {
			if err := file.Close(); err != nil && !strings.Contains(err.Error(), "file already closed") {
				assert.NoError(t, err, "Failed to close test file")
			}
		}()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(nil, errors.New("post failed"))

		p := &poster{
			uploadPool:  mockPool,
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mocks.NewMockJobProgress(ctrl),
		}

		art := &article.Article{
			MessageID:  "test@example.com",
			Subject:    "Test Subject",
			From:       "test@example.com",
			Groups:     []string{"alt.test"},
			PartNumber: 1,
			TotalParts: 1,
			FileName:   "test.txt",
			Date:       time.Now(),
			Offset:     0,
			Size:       uint64(len(content)),
			FileSize:   int64(len(content)),
		}

		err = p.postArticle(ctx, art, file)

		assert.Error(t, err)
		assert.Contains(t, err.Error(), "error posting article")
	})
}

func TestCheckArticle(t *testing.T) {
	t.Run("success", func(t *testing.T) {
		ctx := context.Background()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().Stat(ctx, "test@example.com").Return(&nntppool.StatResult{}, nil)

		p := &poster{
			uploadPool: mockPool,
			verifyPool: mockPool, // Use same pool for checking
			stats:      &Stats{StartTime: time.Now()},
		}

		art := &article.Article{
			MessageID: "test@example.com",
			Groups:    []string{"alt.test"},
		}

		err := p.checkArticle(ctx, art)

		assert.NoError(t, err)
		assert.Equal(t, int64(1), p.stats.ArticlesChecked)
	})

	t.Run("article not found", func(t *testing.T) {
		ctx := context.Background()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().Stat(ctx, "test@example.com").Return(nil, errors.New("not found"))

		p := &poster{
			uploadPool: mockPool,
			verifyPool: mockPool, // Use same pool for checking
			stats:      &Stats{StartTime: time.Now()},
		}

		art := &article.Article{
			MessageID: "test@example.com",
			Groups:    []string{"alt.test"},
		}

		err := p.checkArticle(ctx, art)

		assert.Error(t, err)
		assert.Contains(t, err.Error(), "article not found")
	})
}

func TestAddPost(t *testing.T) {
	t.Run("success", func(t *testing.T) {
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		content := strings.Repeat("test data ", 200) // Make it large enough to create multiple segments
		testFile := createTestFile(t, content)
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		cfg := createTestConfig()
		cfg.ArticleSizeInBytes = 100 // Small segment size to force multiple segments

		// Mock the job progress
		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		p := &poster{
			cfg:         cfg,
			jobProgress: mockJobProgress,
		}

		var wg sync.WaitGroup
		var postsInFlight sync.WaitGroup
		var failedPosts atomic.Int64
		postQueue := make(chan *Post, 10)
		nzbGen := mocks.NewMockNZBGenerator(ctrl)

		wg.Add(1)
		ctx := context.Background()
		err := p.addPost(ctx, testFile, "", 1, 1, &wg, &failedPosts, postQueue, nzbGen, &postsInFlight)

		assert.NoError(t, err)

		// Check that a post was added to the queue
		select {
		case post := <-postQueue:
			assert.Equal(t, testFile, post.FilePath)
			assert.Equal(t, PostStatusPending, post.Status)
			assert.Greater(t, len(post.Articles), 1) // Should have multiple articles due to small segment size
			assert.NotNil(t, post.file)
			assert.Equal(t, int64(len(content)), post.filesize)

			// Clean up
			if err := post.file.Close(); err != nil && !strings.Contains(err.Error(), "file already closed") {
				assert.NoError(t, err, "Failed to close test file")
			}
		default:
			t.Fatal("Expected post to be added to queue")
		}
	})

	t.Run("file not found", func(t *testing.T) {
		p := &poster{
			cfg: createTestConfig(),
		}

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		var wg sync.WaitGroup
		var postsInFlight sync.WaitGroup
		var failedPosts atomic.Int64
		postQueue := make(chan *Post, 10)
		nzbGen := mocks.NewMockNZBGenerator(ctrl)

		ctx := context.Background()
		err := p.addPost(ctx, "nonexistent.txt", "", 1, 1, &wg, &failedPosts, postQueue, nzbGen, &postsInFlight)

		assert.Error(t, err)
		assert.Contains(t, err.Error(), "error opening file")
	})
}

func TestPostStatus(t *testing.T) {
	// Test that PostStatus constants are properly defined
	assert.Equal(t, PostStatus(0), PostStatusPending)
	assert.Equal(t, PostStatus(1), PostStatusPosted)
	assert.Equal(t, PostStatus(2), PostStatusVerified)
	assert.Equal(t, PostStatus(3), PostStatusFailed)
}

func TestPost_ConcurrentAccess(t *testing.T) {
	// Test that Post struct can handle concurrent access safely
	post := &Post{
		FilePath: "test.txt",
		Status:   PostStatusPending,
	}

	var wg sync.WaitGroup
	numGoroutines := 10

	// Simulate concurrent access to the post
	wg.Add(numGoroutines)
	for range numGoroutines {
		go func() {
			defer wg.Done()
			post.mu.Lock()
			defer post.mu.Unlock()

			// Simulate some work with the post
			status := post.Status
			post.Status = PostStatusPosted
			post.Status = status
		}()
	}

	wg.Wait()
	// Test passes if no race conditions occur
}

func TestStats_ConcurrentAccess(t *testing.T) {
	// Test that Stats can handle concurrent access safely
	stats := &Stats{
		StartTime: time.Now(),
	}

	var wg sync.WaitGroup
	numGoroutines := 10
	numOperations := 100

	// Simulate concurrent updates to stats
	wg.Add(numGoroutines)
	for range numGoroutines {
		go func() {
			defer wg.Done()
			for range numOperations {
				stats.mu.Lock()
				stats.ArticlesPosted++
				stats.BytesPosted += 1024
				stats.mu.Unlock()
			}
		}()
	}

	wg.Wait()

	stats.mu.Lock()
	expectedArticles := int64(numGoroutines * numOperations)
	expectedBytes := int64(numGoroutines * numOperations * 1024)
	stats.mu.Unlock()

	assert.Equal(t, expectedArticles, stats.ArticlesPosted)
	assert.Equal(t, expectedBytes, stats.BytesPosted)
}

func TestPosterInterface(t *testing.T) {
	// Test that poster implements the Poster interface
	ctx := context.Background()

	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	mockPool := createMockNNTPClient(ctrl)
	mockPoolManager := createMockPoolManager(ctrl, mockPool)
	mockConfig := mocks.NewMockConfig(ctrl)
	mockConfig.EXPECT().GetPostingConfig().Return(createTestConfig())
	mockConfig.EXPECT().GetPostCheckConfig().Return(createTestPostCheckConfig())
	mockJobProgress := mocks.NewMockJobProgress(ctrl)

	var p Poster
	poster, err := New(ctx, mockConfig, mockPoolManager, mockJobProgress)
	require.NoError(t, err)

	p = poster // This should compile if poster implements Poster interface
	assert.NotNil(t, p)

	// Test that all interface methods are available
	stats := p.Stats()
	assert.NotNil(t, &stats)
}

// Integration test with real files
func TestPostIntegration(t *testing.T) {
	t.Run("small file integration", func(t *testing.T) {
		ctx := context.Background()

		// Create a temporary test file
		tmpDir := t.TempDir()
		testFile := filepath.Join(tmpDir, "test.txt")
		content := "This is a test file for integration testing."

		err := os.WriteFile(testFile, []byte(content), 0644)
		require.NoError(t, err)

		// Mock NNTP pool that always succeeds
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil).AnyTimes()
		mockPool.EXPECT().Stat(gomock.Any(), gomock.Any()).Return(&nntppool.StatResult{}, nil).AnyTimes()

		// Mock NZB generator
		nzbGen := mocks.NewMockNZBGenerator(ctrl)
		nzbGen.EXPECT().AddArticle(gomock.Any()).Return()

		// Create poster with realistic config
		cfg := createTestConfig()
		cfg.ArticleSizeInBytes = 1000 // Larger than our test file

		checkCfg := createTestPostCheckConfig()
		enabled := true
		checkCfg.Enabled = &enabled
		checkCfg.RetryDelay = config.Duration("0s") // avoid real sleep in tests

		// Mock the job progress
		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		p := &poster{
			cfg:         cfg,
			checkCfg:    checkCfg,
			uploadPool:  mockPool,
			verifyPool:  mockPool, // Use same pool for checking
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mockJobProgress,
		}

		// Post the file
		err = p.Post(ctx, []string{testFile}, tmpDir, nzbGen)

		assert.NoError(t, err)

		// Verify stats were updated
		stats := p.Stats()
		assert.Equal(t, int64(1), stats.ArticlesPosted)
		assert.Equal(t, int64(1), stats.ArticlesChecked)
		assert.Equal(t, int64(len(content)), stats.BytesPosted)
	})
}

// Simplified unit tests for individual methods instead of complex integration tests

func TestPostLoop_Basic(t *testing.T) {
	t.Run("postLoop processes single article successfully", func(t *testing.T) {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()

		content := "test content for post loop"
		testFile := createTestFile(t, content)
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil).Times(1)

		// Mock the job progress
		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		p := &poster{
			cfg:         createTestConfig(),
			checkCfg:    createTestPostCheckConfig(),
			uploadPool:  mockPool,
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mockJobProgress,
		}

		// Test postArticle directly instead of the full loop
		file, err := os.Open(testFile)
		require.NoError(t, err)
		defer func() {
			if err := file.Close(); err != nil && !strings.Contains(err.Error(), "file already closed") {
				assert.NoError(t, err, "Failed to close test file")
			}
		}()

		art := &article.Article{
			MessageID:    "test@example.com",
			Subject:      "Test Subject",
			From:         "test@example.com",
			Groups:       []string{"alt.test"},
			PartNumber:   1,
			TotalParts:   1,
			FileName:     "test.txt",
			OriginalName: "test.txt",
			Date:         time.Now(),
			Offset:       0,
			Size:         uint64(len(content)),
			FileSize:     int64(len(content)),
		}

		err = p.postArticle(ctx, art, file)
		assert.NoError(t, err)
		assert.Equal(t, int64(1), p.stats.ArticlesPosted)
	})

	t.Run("postLoop handles posting error", func(t *testing.T) {
		ctx := context.Background()
		content := "test content"
		testFile := createTestFile(t, content)
		defer func() {
			err := os.Remove(testFile)
			assert.NoError(t, err, "Failed to remove test file")
		}()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(nil, errors.New("posting failed"))

		// Mock the job progress
		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		p := &poster{
			uploadPool:  mockPool,
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mockJobProgress,
		}

		file, err := os.Open(testFile)
		require.NoError(t, err)
		defer func() {
			if err := file.Close(); err != nil && !strings.Contains(err.Error(), "file already closed") {
				assert.NoError(t, err, "Failed to close test file")
			}
		}()

		art := &article.Article{
			MessageID:    "test@example.com",
			Subject:      "Test Subject",
			From:         "test@example.com",
			Groups:       []string{"alt.test"},
			PartNumber:   1,
			TotalParts:   1,
			FileName:     "test.txt",
			OriginalName: "test.txt",
			Date:         time.Now(),
			Offset:       0,
			Size:         uint64(len(content)),
			FileSize:     int64(len(content)),
		}

		err = p.postArticle(ctx, art, file)
		assert.Error(t, err)
		assert.Contains(t, err.Error(), "error posting article")
	})

	t.Run("postArticleWithBody retries once on broken pipe and succeeds", func(t *testing.T) {
		ctx := context.Background()
		content := "test content"

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		// First call returns broken pipe; second call succeeds.
		gomock.InOrder(
			mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).
				Return(nil, fmt.Errorf("write tcp: %w", syscall.EPIPE)),
			mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).
				Return(&nntppool.PostResult{}, nil),
		)

		p := &poster{
			uploadPool: mockPool,
			stats:      &Stats{StartTime: time.Now()},
		}

		art := &article.Article{
			MessageID:    "test@example.com",
			Subject:      "Test Subject",
			From:         "test@example.com",
			Groups:       []string{"alt.test"},
			PartNumber:   1,
			TotalParts:   1,
			FileName:     "test.txt",
			OriginalName: "test.txt",
			Date:         time.Now(),
			Size:         uint64(len(content)),
			FileSize:     int64(len(content)),
		}

		err := p.postArticleWithBody(ctx, art, []byte(content))
		assert.NoError(t, err)
		assert.Equal(t, int64(1), p.stats.ArticlesPosted)
	})

	t.Run("postArticleWithBody does not retry on non-broken-pipe error", func(t *testing.T) {
		ctx := context.Background()
		content := "test content"

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		// Only one call expected — non-broken-pipe errors are not retried.
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).
			Return(nil, errors.New("441 posting failed")).Times(1)

		p := &poster{
			uploadPool: mockPool,
			stats:      &Stats{StartTime: time.Now()},
		}

		art := &article.Article{
			MessageID:    "test@example.com",
			Subject:      "Test Subject",
			From:         "test@example.com",
			Groups:       []string{"alt.test"},
			PartNumber:   1,
			TotalParts:   1,
			FileName:     "test.txt",
			OriginalName: "test.txt",
			Date:         time.Now(),
			Size:         uint64(len(content)),
			FileSize:     int64(len(content)),
		}

		err := p.postArticleWithBody(ctx, art, []byte(content))
		assert.Error(t, err)
		assert.Contains(t, err.Error(), "error posting article")
	})

	t.Run("postArticleWithBody fails when all attempts return broken pipe", func(t *testing.T) {
		ctx := context.Background()
		content := "test content"

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).
			Return(nil, fmt.Errorf("write tcp: %w", syscall.EPIPE)).Times(3)

		p := &poster{
			uploadPool: mockPool,
			stats:      &Stats{StartTime: time.Now()},
		}

		art := &article.Article{
			MessageID:    "test@example.com",
			Subject:      "Test Subject",
			From:         "test@example.com",
			Groups:       []string{"alt.test"},
			PartNumber:   1,
			TotalParts:   1,
			FileName:     "test.txt",
			OriginalName: "test.txt",
			Date:         time.Now(),
			Size:         uint64(len(content)),
			FileSize:     int64(len(content)),
		}

		err := p.postArticleWithBody(ctx, art, []byte(content))
		assert.Error(t, err)
		assert.Contains(t, err.Error(), "error posting article")
	})
}

func TestCheckLoop_Basic(t *testing.T) {
	t.Run("checkArticle verifies successfully", func(t *testing.T) {
		ctx := context.Background()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().Stat(ctx, "test@example.com").Return(&nntppool.StatResult{}, nil)

		p := &poster{
			uploadPool: mockPool,
			verifyPool: mockPool, // Use same pool for checking
			stats:      &Stats{StartTime: time.Now()},
		}

		art := &article.Article{
			MessageID: "test@example.com",
			Groups:    []string{"alt.test"},
		}

		err := p.checkArticle(ctx, art)
		assert.NoError(t, err)
		assert.Equal(t, int64(1), p.stats.ArticlesChecked)
	})

	t.Run("checkArticle handles verification failure", func(t *testing.T) {
		ctx := context.Background()

		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
		}).AnyTimes()
		mockPool.EXPECT().Stat(ctx, "test@example.com").Return(nil, errors.New("article not found"))

		p := &poster{
			uploadPool: mockPool,
			verifyPool: mockPool, // Use same pool for checking
			stats:      &Stats{StartTime: time.Now()},
		}

		art := &article.Article{
			MessageID: "test@example.com",
			Groups:    []string{"alt.test"},
		}

		err := p.checkArticle(ctx, art)
		assert.Error(t, err)
		assert.Contains(t, err.Error(), "article not found")
		assert.Equal(t, int64(0), p.stats.ArticlesChecked)
	})
}

// Test helper functions

// TestSharedUploadPool verifies that the global poster worker pool introduced
// for issue #184 (https://github.com/kipsilabs/postie/issues/184) actually caps
// concurrent in-flight article posts at numOfConnections regardless of how
// many concurrent Post() calls are happening, and that Close() drains cleanly.
func TestSharedUploadPool(t *testing.T) {
	t.Run("concurrent posts share workers and stay bounded", func(t *testing.T) {
		ctx := context.Background()
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		const maxConnections = 4
		const concurrentPosts = 8

		// Track simultaneously-active article posts. The mock blocks briefly so
		// many articles overlap; the in-flight peak must not exceed
		// maxConnections.
		var inflight atomic.Int32
		var peak atomic.Int32

		mockPool := mocks.NewMockNNTPClient(ctrl)
		mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
			Providers: []nntppool.ProviderStats{{MaxConnections: maxConnections}},
		}).AnyTimes()
		mockPool.EXPECT().
			PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).
			DoAndReturn(func(_ context.Context, _ any, _ any, _ any) (*nntppool.PostResult, error) {
				cur := inflight.Add(1)
				for {
					p := peak.Load()
					if cur <= p || peak.CompareAndSwap(p, cur) {
						break
					}
				}
				time.Sleep(5 * time.Millisecond)
				inflight.Add(-1)
				return &nntppool.PostResult{}, nil
			}).
			AnyTimes()

		nzbGen := mocks.NewMockNZBGenerator(ctrl)
		nzbGen.EXPECT().AddArticle(gomock.Any()).Return().AnyTimes()

		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		checkCfg := createTestPostCheckConfig()
		disabled := false
		checkCfg.Enabled = &disabled

		p := &poster{
			cfg:         createTestConfig(),
			checkCfg:    checkCfg,
			uploadPool:  mockPool,
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mockJobProgress,
		}

		// Build a content blob large enough to produce several articles per
		// Post() so workers actually contend for slots.
		content := strings.Repeat("test data ", 500)

		var wg sync.WaitGroup
		errCh := make(chan error, concurrentPosts)
		for range concurrentPosts {
			testFile := createTestFile(t, content)
			defer func() { _ = os.Remove(testFile) }()
			wg.Add(1)
			go func(f string) {
				defer wg.Done()
				errCh <- p.Post(ctx, []string{f}, "", nzbGen)
			}(testFile)
		}
		wg.Wait()
		close(errCh)
		for err := range errCh {
			assert.NoError(t, err)
		}

		assert.LessOrEqual(t, int(peak.Load()), maxConnections,
			"peak concurrent in-flight article posts should not exceed numOfConnections")
		assert.Equal(t, int32(0), inflight.Load(),
			"all in-flight posts should have completed before Wait() returned")

		p.Close()
	})

	t.Run("Close drains workers and rejects new Post calls", func(t *testing.T) {
		ctx := context.Background()
		ctrl := gomock.NewController(t)
		defer ctrl.Finish()

		mockPool := createMockNNTPClient(ctrl)
		mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(&nntppool.PostResult{}, nil).AnyTimes()

		nzbGen := mocks.NewMockNZBGenerator(ctrl)
		nzbGen.EXPECT().AddArticle(gomock.Any()).Return().AnyTimes()

		mockJobProgress := mocks.NewMockJobProgress(ctrl)
		mockProgress := mocks.NewMockProgress(ctrl)
		mockJobProgress.EXPECT().AddProgress(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).Return(mockProgress).AnyTimes()
		mockJobProgress.EXPECT().FinishProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().UpdateProgress(gomock.Any()).AnyTimes()
		mockProgress.EXPECT().Finish().AnyTimes()
		mockProgress.EXPECT().GetID().Return(uuid.New()).AnyTimes()

		checkCfg := createTestPostCheckConfig()
		disabled := false
		checkCfg.Enabled = &disabled

		p := &poster{
			cfg:         createTestConfig(),
			checkCfg:    checkCfg,
			uploadPool:  mockPool,
			stats:       &Stats{StartTime: time.Now()},
			throttle:    NewThrottle(1024*1024, time.Second),
			jobProgress: mockJobProgress,
		}

		testFile := createTestFile(t, strings.Repeat("test data ", 100))
		defer func() { _ = os.Remove(testFile) }()

		require.NoError(t, p.Post(ctx, []string{testFile}, "", nzbGen))

		// Close must drain workers (no goroutine leak) and short-circuit
		// subsequent Post() calls with ErrPosterClosed.
		closed := make(chan struct{})
		go func() {
			p.Close()
			close(closed)
		}()
		select {
		case <-closed:
		case <-time.After(2 * time.Second):
			t.Fatal("Close did not drain workers within 2s")
		}

		err := p.Post(ctx, []string{testFile}, "", nzbGen)
		assert.ErrorIs(t, err, ErrPosterClosed)
	})
}

func createTestConfig() config.PostingConfig {
	enabled := true
	return config.PostingConfig{
		MaxRetries:         3,
		RetryDelay:         config.Duration("1s"),
		ArticleSizeInBytes: 1000,
		Groups:             []config.NewsgroupConfig{{Name: "alt.test", Enabled: &enabled}},
		ThrottleRate:       1024 * 1024,
		MessageIDFormat:    config.MessageIDFormatRandom,
		PostHeaders: config.PostHeaders{
			AddNXGHeader: false,
			DefaultFrom:  "",
		},
		ObfuscationPolicy:     config.ObfuscationPolicyNone,
		Par2ObfuscationPolicy: config.ObfuscationPolicyNone,
		GroupPolicy:           config.GroupPolicyEachFile,
		WaitForPar2:           &enabled,
	}
}

func createTestPostCheckConfig() config.PostCheck {
	enabled := true
	return config.PostCheck{
		Enabled:    &enabled,
		RetryDelay: config.Duration("1s"),
		MaxRePost:  2,
	}
}

func createTestFile(t *testing.T, content string) string {
	tmpFile, err := os.CreateTemp("", "test_*.txt")
	require.NoError(t, err)

	_, err = tmpFile.WriteString(content)
	require.NoError(t, err)

	err = tmpFile.Close()
	require.NoError(t, err)

	return tmpFile.Name()
}

// createMockNNTPClient creates a mock NNTPClient with default Stats expectations
func createMockNNTPClient(ctrl *gomock.Controller) *mocks.MockNNTPClient {
	mockPool := mocks.NewMockNNTPClient(ctrl)
	mockPool.EXPECT().Stats().Return(nntppool.ClientStats{
		Providers: []nntppool.ProviderStats{{MaxConnections: 10}},
	}).AnyTimes()
	return mockPool
}

// createMockPoolManager creates a mock pool manager for testing using the generated mock
func createMockPoolManager(ctrl *gomock.Controller, nntpClient pool.NNTPClient) *mocks.MockPoolManager {
	mockPoolManager := mocks.NewMockPoolManager(ctrl)
	mockPoolManager.EXPECT().GetUploadPool().Return(nntpClient).AnyTimes()
	mockPoolManager.EXPECT().GetVerifyPool().Return(nntpClient).AnyTimes()
	return mockPoolManager
}
