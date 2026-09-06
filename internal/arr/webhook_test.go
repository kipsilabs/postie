package arr_test

import (
	"testing"

	"github.com/kipsilabs/postie/internal/arr"
	"github.com/stretchr/testify/assert"
)

func TestExtractFilePaths_Radarr(t *testing.T) {
	payload := arr.WebhookPayload{
		EventType: "Download",
		MovieFile: &arr.MovieFilePayload{Path: "/movies/The.Matrix.1999.mkv"},
	}
	got := arr.ExtractFilePaths(payload)
	assert.Equal(t, []string{"/movies/The.Matrix.1999.mkv"}, got)
}

func TestExtractFilePaths_Sonarr(t *testing.T) {
	payload := arr.WebhookPayload{
		EventType:   "Download",
		EpisodeFile: &arr.EpisodeFilePayload{Path: "/tv/Breaking.Bad.S01E01.mkv"},
	}
	got := arr.ExtractFilePaths(payload)
	assert.Equal(t, []string{"/tv/Breaking.Bad.S01E01.mkv"}, got)
}

func TestExtractFilePaths_Lidarr(t *testing.T) {
	payload := arr.WebhookPayload{
		EventType: "Download",
		TrackFiles: []arr.TrackFilePayload{
			{Path: "/music/Artist/01.flac"},
			{Path: "/music/Artist/02.flac"},
		},
	}
	got := arr.ExtractFilePaths(payload)
	assert.Equal(t, []string{"/music/Artist/01.flac", "/music/Artist/02.flac"}, got)
}

func TestExtractFilePaths_Readarr(t *testing.T) {
	payload := arr.WebhookPayload{
		EventType: "Download",
		BookFiles: []arr.BookFilePayload{
			{Path: "/books/Author/Book.epub"},
		},
	}
	got := arr.ExtractFilePaths(payload)
	assert.Equal(t, []string{"/books/Author/Book.epub"}, got)
}

func TestExtractFilePaths_TestEventIgnored(t *testing.T) {
	payload := arr.WebhookPayload{
		EventType: "Test",
		MovieFile: &arr.MovieFilePayload{Path: "/movies/some.mkv"},
	}
	got := arr.ExtractFilePaths(payload)
	assert.Empty(t, got)
}
