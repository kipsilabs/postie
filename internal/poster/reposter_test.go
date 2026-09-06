package poster

import (
	"context"
	"io"
	"os"
	"path/filepath"
	"testing"

	"github.com/javi11/nntppool/v4"
	"github.com/mnightingale/rapidyenc"
	"go.uber.org/mock/gomock"

	"github.com/kipsilabs/postie/internal/manifest"
	"github.com/kipsilabs/postie/internal/mocks"
)

func TestReposter_RepostReusesMessageIDAndBody(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	// Source file with a known article body at a known offset.
	dir := t.TempDir()
	src := filepath.Join(dir, "src.bin")
	content := []byte("AAAAAAAAAABBBBBBBBBB") // 20 bytes, two 10-byte articles
	if err := os.WriteFile(src, content, 0o644); err != nil {
		t.Fatal(err)
	}

	rec := manifest.ArticleRecord{
		Index:      1,
		SourcePath: src,
		MessageID:  "part2@postie",
		Subject:    "subj",
		From:       "from",
		Groups:     []string{"alt.binaries.test"},
		Offset:     10,
		BodySize:   10,
		FileName:   "src.bin",
		PartNumber: 2,
		TotalParts: 2,
	}

	var gotHeaders nntppool.PostHeaders
	var gotBody []byte
	mockPool := mocks.NewMockNNTPClient(ctrl)
	mockPool.EXPECT().
		PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).
		DoAndReturn(func(_ context.Context, h nntppool.PostHeaders, body io.Reader, _ rapidyenc.Meta) (*nntppool.PostResult, error) {
			gotHeaders = h
			gotBody, _ = io.ReadAll(body)
			return &nntppool.PostResult{}, nil
		})

	r := NewReposter(mockPool, nil, 0)
	if err := r.Repost(context.Background(), rec); err != nil {
		t.Fatalf("Repost: %v", err)
	}

	if gotHeaders.MessageID != "<part2@postie>" {
		t.Errorf("MessageID = %q, want <part2@postie>", gotHeaders.MessageID)
	}
	if string(gotBody) != "BBBBBBBBBB" {
		t.Errorf("body = %q, want the second 10-byte article", string(gotBody))
	}
	if r.Stats().ArticlesPosted != 1 {
		t.Errorf("ArticlesPosted = %d, want 1", r.Stats().ArticlesPosted)
	}
}

func TestReposter_MissingSourceFileErrors(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()
	mockPool := mocks.NewMockNNTPClient(ctrl) // PostYenc must NOT be called

	r := NewReposter(mockPool, nil, 0)
	err := r.Repost(context.Background(), manifest.ArticleRecord{
		SourcePath: "/nonexistent/file.bin", MessageID: "x", BodySize: 10,
	})
	if err == nil {
		t.Fatal("expected error for missing source file")
	}
}

func TestReposter_WorksWithSharedEngine(t *testing.T) {
	ctrl := gomock.NewController(t)
	defer ctrl.Finish()

	dir := t.TempDir()
	src := filepath.Join(dir, "s.bin")
	if err := os.WriteFile(src, []byte("0123456789"), 0o644); err != nil {
		t.Fatal(err)
	}

	mockPool := mocks.NewMockNNTPClient(ctrl)
	mockPool.EXPECT().PostYenc(gomock.Any(), gomock.Any(), gomock.Any(), gomock.Any()).
		Return(&nntppool.PostResult{}, nil)

	// A real engine exercises the worker/buffer reservation path during re-post.
	eng := NewEngine(768*1024, 0, 4)
	r := NewReposter(mockPool, eng, 0)

	if err := r.Repost(context.Background(), manifest.ArticleRecord{
		SourcePath: src, MessageID: "m", Offset: 0, BodySize: 10,
	}); err != nil {
		t.Fatalf("Repost: %v", err)
	}
	// Engine resources must be fully released after the re-post.
	if m := eng.Metrics(); m.ActiveWorkers != 0 || m.ReservedBytes != 0 {
		t.Errorf("engine not drained: %+v", m)
	}
}

func TestReposter_NoPoolErrors(t *testing.T) {
	r := NewReposter(nil, nil, 0)
	if err := r.Repost(context.Background(), manifest.ArticleRecord{}); err == nil {
		t.Error("expected error when reposter has no upload pool")
	}
}
