package nzb

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/kipsilabs/postie/internal/article"
	"github.com/kipsilabs/postie/internal/config"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewGenerator(t *testing.T) {
	segmentSize := uint64(1000)
	compressionConfig := config.NzbCompressionConfig{
		Enabled: false,
		Type:    config.CompressionTypeNone,
		Level:   0,
	}

	generator := NewGenerator(segmentSize, compressionConfig, true)

	assert.NotNil(t, generator, "Generator should not be nil")

	// Check that the generator was initialized with the correct values
	g, ok := generator.(*Generator)
	require.True(t, ok, "Generator should be of type *Generator")
	assert.Equal(t, segmentSize, g.segmentSize, "Segment size should match")
	assert.Equal(t, compressionConfig, g.compressionConfig, "Compression config should match")
	assert.NotNil(t, g.articles, "Articles map should be initialized")
	assert.NotNil(t, g.filesHash, "Files hash map should be initialized")
}

func TestAddArticle(t *testing.T) {
	segmentSize := uint64(1000)
	compressionConfig := config.NzbCompressionConfig{
		Enabled: false,
		Type:    config.CompressionTypeNone,
		Level:   0,
	}

	generator := NewGenerator(segmentSize, compressionConfig, true).(*Generator)

	// Create a test article
	testArticle := &article.Article{
		MessageID:       "test-message-id-1",
		OriginalName:    "test-file.txt",
		OriginalSubject: "Test Subject",
		From:            "test@example.com",
		Groups:          []string{"alt.test"},
		PartNumber:      1,
		TotalParts:      2,
		Size:            500,
		FileNumber:      1,
	}

	// Add the article to the generator
	generator.AddArticle(testArticle)

	// Check that the article was added correctly
	assert.Len(t, generator.articles, 1, "Generator should have one file")
	assert.Len(t, generator.articles["test-file.txt"], 1, "File should have one article")
	assert.Equal(t, testArticle, generator.articles["test-file.txt"][0], "Article should match")

	// Create a second article with the same message ID but different data
	updatedArticle := &article.Article{
		MessageID:       "test-message-id-1", // Same message ID
		OriginalName:    "test-file.txt",
		OriginalSubject: "Updated Subject",
		From:            "updated@example.com",
		Groups:          []string{"alt.updated"},
		PartNumber:      1,
		TotalParts:      2,
		Size:            600,
		FileNumber:      1,
	}

	// Add the updated article
	generator.AddArticle(updatedArticle)

	// Check that the article was updated rather than added
	assert.Len(t, generator.articles, 1, "Generator should still have one file")
	assert.Len(t, generator.articles["test-file.txt"], 1, "File should still have one article")
	assert.Equal(t, updatedArticle, generator.articles["test-file.txt"][0], "Article should be updated")

	// Add another article with a different message ID
	secondArticle := &article.Article{
		MessageID:       "test-message-id-2",
		OriginalName:    "test-file.txt",
		OriginalSubject: "Second Article",
		From:            "second@example.com",
		Groups:          []string{"alt.second"},
		PartNumber:      2,
		TotalParts:      2,
		Size:            500,
		FileNumber:      1,
	}

	// Add the second article
	generator.AddArticle(secondArticle)

	// Check that both articles are now in the generator
	assert.Len(t, generator.articles, 1, "Generator should have one file")
	assert.Len(t, generator.articles["test-file.txt"], 2, "File should have two articles")
}

func TestAddFileHash(t *testing.T) {
	segmentSize := uint64(1000)
	compressionConfig := config.NzbCompressionConfig{
		Enabled: false,
		Type:    config.CompressionTypeNone,
		Level:   0,
	}

	generator := NewGenerator(segmentSize, compressionConfig, true).(*Generator)

	// Add a file hash
	filename := "test-file.txt"
	hash := "abcdef1234567890"

	generator.AddFileHash(filename, hash)

	// Check that the hash was added correctly
	assert.Equal(t, hash, generator.filesHash[filename], "File hash should match")
}

func TestGenerate(t *testing.T) {
	t.Run("generate without compression", func(t *testing.T) {
		segmentSize := uint64(1000)
		compressionConfig := config.NzbCompressionConfig{
			Enabled: false,
			Type:    config.CompressionTypeNone,
			Level:   0,
		}

		generator := NewGenerator(segmentSize, compressionConfig, true).(*Generator)

		// Add test articles
		testArticle1 := &article.Article{
			MessageID:       "test-message-id-1",
			OriginalName:    "test-file.txt",
			OriginalSubject: "Test Subject",
			From:            "test@example.com",
			Groups:          []string{"alt.test"},
			PartNumber:      1,
			TotalParts:      2,
			Size:            500,
			FileNumber:      1,
			FileName:        "test-file.txt",
		}

		testArticle2 := &article.Article{
			MessageID:       "test-message-id-2",
			OriginalName:    "test-file.txt",
			OriginalSubject: "Test Subject",
			From:            "test@example.com",
			Groups:          []string{"alt.test"},
			PartNumber:      2,
			TotalParts:      2,
			Size:            500,
			FileNumber:      1,
			FileName:        "test-file.txt",
		}

		generator.AddArticle(testArticle1)
		generator.AddArticle(testArticle2)

		// Add a file hash
		hash := "abcdef1234567890"
		generator.AddFileHash("test-file.txt", hash)

		// Create a temporary directory for the NZB file
		tempDir, err := os.MkdirTemp("", "nzb-test")
		require.NoError(t, err, "Failed to create temp directory")
		defer func() {
			err := os.RemoveAll(tempDir)
			assert.NoError(t, err, "Failed to remove temp directory")
		}()

		outputPath := filepath.Join(tempDir, "test.nzb")

		// Generate the NZB file
		finalPath, err := generator.Generate(outputPath)
		require.NoError(t, err, "Failed to generate NZB file")

		// Check that the NZB file was created
		_, err = os.Stat(finalPath)
		assert.NoError(t, err, "NZB file should exist")

		// Parse the NZB file to check its contents
		nzbFile, err := Parse(finalPath)
		require.NoError(t, err, "Failed to parse NZB file")

		// Check the NZB file contents
		assert.Len(t, nzbFile.Files, 1, "NZB should have one file")
		assert.Equal(t, testArticle1.OriginalSubject, nzbFile.Files[0].Subject, "Subject should match")
		assert.Equal(t, testArticle1.Groups, nzbFile.Files[0].Groups, "Groups should match")
		assert.Equal(t, testArticle1.From, nzbFile.Files[0].Poster, "Poster should match")
		assert.Equal(t, hash, nzbFile.Files[0].FileHash, "File hash should match")
		assert.Len(t, nzbFile.Files[0].Segments, 2, "File should have two segments")
		assert.Equal(t, 1, nzbFile.Files[0].Segments[0].Number, "First segment number should be 1")
		assert.Equal(t, 2, nzbFile.Files[0].Segments[1].Number, "Second segment number should be 2")
	})

	t.Run("generate with zstd compression", func(t *testing.T) {
		segmentSize := uint64(1000)
		compressionConfig := config.NzbCompressionConfig{
			Enabled: true,
			Type:    config.CompressionTypeZstd,
			Level:   3,
		}

		generator := NewGenerator(segmentSize, compressionConfig, true).(*Generator)

		// Add test articles
		testArticle1 := &article.Article{
			MessageID:       "test-message-id-1",
			OriginalName:    "test-file.txt",
			OriginalSubject: "Test Subject",
			From:            "test@example.com",
			Groups:          []string{"alt.test"},
			PartNumber:      1,
			TotalParts:      2,
			Size:            500,
			FileNumber:      1,
		}

		generator.AddArticle(testArticle1)

		// Create a temporary directory for the NZB file
		tempDir, err := os.MkdirTemp("", "nzb-test")
		require.NoError(t, err, "Failed to create temp directory")
		defer func() {
			err := os.RemoveAll(tempDir)
			assert.NoError(t, err, "Failed to remove temp directory")
		}()

		outputPath := filepath.Join(tempDir, "test.nzb")

		// Generate the NZB file
		finalPath, err := generator.Generate(outputPath)
		require.NoError(t, err, "Failed to generate NZB file")

		// Check that the compressed NZB file was created
		_, err = os.Stat(finalPath)
		assert.NoError(t, err, "Compressed NZB file should exist")
	})

	t.Run("generate with brotli compression", func(t *testing.T) {
		segmentSize := uint64(1000)
		compressionConfig := config.NzbCompressionConfig{
			Enabled: true,
			Type:    config.CompressionTypeBrotli,
			Level:   4,
		}

		generator := NewGenerator(segmentSize, compressionConfig, true).(*Generator)

		// Add test articles
		testArticle1 := &article.Article{
			MessageID:       "test-message-id-1",
			OriginalName:    "test-file.txt",
			OriginalSubject: "Test Subject",
			From:            "test@example.com",
			Groups:          []string{"alt.test"},
			PartNumber:      1,
			TotalParts:      2,
			Size:            500,
			FileNumber:      1,
		}

		generator.AddArticle(testArticle1)

		// Create a temporary directory for the NZB file
		tempDir, err := os.MkdirTemp("", "nzb-test")
		require.NoError(t, err, "Failed to create temp directory")
		defer func() {
			err := os.RemoveAll(tempDir)
			assert.NoError(t, err, "Failed to remove temp directory")
		}()

		outputPath := filepath.Join(tempDir, "test.nzb")

		// Generate the NZB file
		finalPath, err := generator.Generate(outputPath)
		require.NoError(t, err, "Failed to generate NZB file")

		// Check that the compressed NZB file was created
		_, err = os.Stat(finalPath)
		assert.NoError(t, err, "Compressed NZB file should exist")
	})

	t.Run("folder name with dots does not get truncated at first dot", func(t *testing.T) {
		segmentSize := uint64(1000)
		compressionConfig := config.NzbCompressionConfig{
			Enabled: false,
			Type:    config.CompressionTypeNone,
			Level:   0,
		}

		// maintainOriginalExtension=false triggers the old buggy path that split on dots
		generator := NewGenerator(segmentSize, compressionConfig, false).(*Generator)

		testArticle := &article.Article{
			MessageID:       "test-message-id-1",
			OriginalName:    "file.rar",
			OriginalSubject: "Test Subject",
			From:            "test@example.com",
			Groups:          []string{"alt.test"},
			PartNumber:      1,
			TotalParts:      1,
			Size:            500,
			FileNumber:      1,
			FileName:        "file.rar",
		}
		generator.AddArticle(testArticle)

		tempDir, err := os.MkdirTemp("", "nzb-test")
		require.NoError(t, err)
		defer func() {
			_ = os.RemoveAll(tempDir)
		}()

		outputPath := filepath.Join(tempDir, "Sun.Moments.nzb")

		finalPath, err := generator.Generate(outputPath)
		require.NoError(t, err, "Generate should succeed")

		assert.Equal(t, outputPath, finalPath, "Returned path must equal the requested output path")

		_, err = os.Stat(outputPath)
		assert.NoError(t, err, "NZB file should exist at Sun.Moments.nzb")

		// Regression guard: the old bug would create Sun.nzb instead
		truncatedPath := filepath.Join(tempDir, "Sun.nzb")
		_, statErr := os.Stat(truncatedPath)
		assert.True(t, os.IsNotExist(statErr), "Sun.nzb must NOT exist (regression guard)")
	})

	t.Run("generate with no articles", func(t *testing.T) {
		segmentSize := uint64(1000)
		compressionConfig := config.NzbCompressionConfig{
			Enabled: false,
			Type:    config.CompressionTypeNone,
			Level:   0,
		}

		generator := NewGenerator(segmentSize, compressionConfig, true).(*Generator)

		// Create a temporary directory for the NZB file
		tempDir, err := os.MkdirTemp("", "nzb-test")
		require.NoError(t, err, "Failed to create temp directory")
		defer func() {
			err := os.RemoveAll(tempDir)
			assert.NoError(t, err, "Failed to remove temp directory")
		}()

		outputPath := filepath.Join(tempDir, "test.nzb")

		// Generate the NZB file
		_, err = generator.Generate(outputPath)
		assert.Error(t, err, "Generate should fail with no articles")
		assert.Contains(t, err.Error(), "no articles found", "Error message should indicate no articles")
	})
}

func TestParse(t *testing.T) {
	// Create a simple NZB file for testing
	nzbContent := `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head>
    <meta type="date">2023-01-01T12:00:00Z</meta>
    <meta type="chunk_size">1000</meta>
  </head>
  <file poster="test@example.com" date="1672574400" subject="Test Subject">
    <groups>
      <group>alt.test</group>
    </groups>
    <segments>
      <segment bytes="500" number="1">test-message-id-1</segment>
      <segment bytes="500" number="2">test-message-id-2</segment>
    </segments>
  </file>
</nzb>`

	// Create a temporary file
	tempFile, err := os.CreateTemp("", "test-*.nzb")
	require.NoError(t, err, "Failed to create temp file")
	defer func() {
		err := os.Remove(tempFile.Name())
		assert.NoError(t, err, "Failed to remove temp file")
	}()

	// Write the NZB content to the temp file
	_, err = tempFile.WriteString(nzbContent)
	require.NoError(t, err, "Failed to write to temp file")
	err = tempFile.Close()
	require.NoError(t, err, "Failed to close temp file")

	// Parse the NZB file
	nzbFile, err := Parse(tempFile.Name())
	require.NoError(t, err, "Failed to parse NZB file")

	// Check the parsed NZB file
	assert.NotNil(t, nzbFile, "Parsed NZB file should not be nil")
	assert.Len(t, nzbFile.Files, 1, "NZB should have one file")
	assert.Equal(t, "Test Subject", nzbFile.Files[0].Subject, "Subject should match")
	assert.Equal(t, []string{"alt.test"}, nzbFile.Files[0].Groups, "Groups should match")
	assert.Equal(t, "test@example.com", nzbFile.Files[0].Poster, "Poster should match")
	assert.Len(t, nzbFile.Files[0].Segments, 2, "File should have two segments")

	// Test parsing a non-existent file
	_, err = Parse("non-existent-file.nzb")
	assert.Error(t, err, "Parsing a non-existent file should fail")
}

func TestValidate(t *testing.T) {
	t.Run("valid nzb file", func(t *testing.T) {
		// Create a valid NZB file for testing
		nzbContent := `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head>
    <meta type="date">2023-01-01T12:00:00Z</meta>
    <meta type="chunk_size">1000</meta>
  </head>
  <file poster="test@example.com" date="1672574400" subject="Test Subject" filename="test.txt">
    <groups>
      <group>alt.test</group>
    </groups>
    <segments>
      <segment bytes="500" number="1">test-message-id-1</segment>
      <segment bytes="500" number="2">test-message-id-2</segment>
    </segments>
  </file>
</nzb>`

		// Create a temporary file
		tempFile, err := os.CreateTemp("", "test-valid-*.nzb")
		require.NoError(t, err, "Failed to create temp file")
		defer func() {
			err := os.Remove(tempFile.Name())
			assert.NoError(t, err, "Failed to remove temp file")
		}()

		// Write the NZB content to the temp file
		_, err = tempFile.WriteString(nzbContent)
		require.NoError(t, err, "Failed to write to temp file")
		err = tempFile.Close()
		require.NoError(t, err, "Failed to close temp file")

		// Validate the NZB file
		err = Validate(tempFile.Name())
		assert.NoError(t, err, "Valid NZB file should pass validation")
	})

	t.Run("nzb file with no files", func(t *testing.T) {
		// Create an NZB file with no files for testing
		nzbContent := `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head>
    <meta type="date">2023-01-01T12:00:00Z</meta>
    <meta type="chunk_size">1000</meta>
  </head>
</nzb>`

		// Create a temporary file
		tempFile, err := os.CreateTemp("", "test-no-files-*.nzb")
		require.NoError(t, err, "Failed to create temp file")
		defer func() {
			err := os.Remove(tempFile.Name())
			assert.NoError(t, err, "Failed to remove temp file")
		}()

		// Write the NZB content to the temp file
		_, err = tempFile.WriteString(nzbContent)
		require.NoError(t, err, "Failed to write to temp file")
		err = tempFile.Close()
		require.NoError(t, err, "Failed to close temp file")

		// Validate the NZB file
		err = Validate(tempFile.Name())
		assert.Error(t, err, "NZB file with no files should fail validation")
		assert.Contains(t, err.Error(), "NZB file contains no files", "Error message should indicate no files")
	})

	t.Run("nzb file with file that has no segments", func(t *testing.T) {
		// Create an NZB file with a file that has no segments
		nzbContent := `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head>
    <meta type="date">2023-01-01T12:00:00Z</meta>
    <meta type="chunk_size">1000</meta>
  </head>
  <file poster="test@example.com" date="1672574400" subject="Test Subject" filename="test.txt">
    <groups>
      <group>alt.test</group>
    </groups>
    <segments>
    </segments>
  </file>
</nzb>`

		// Create a temporary file
		tempFile, err := os.CreateTemp("", "test-no-segments-*.nzb")
		require.NoError(t, err, "Failed to create temp file")
		defer func() {
			err := os.Remove(tempFile.Name())
			assert.NoError(t, err, "Failed to remove temp file")
		}()

		// Write the NZB content to the temp file
		_, err = tempFile.WriteString(nzbContent)
		require.NoError(t, err, "Failed to write to temp file")
		err = tempFile.Close()
		require.NoError(t, err, "Failed to close temp file")

		// Validate the NZB file
		err = Validate(tempFile.Name())
		assert.Error(t, err, "NZB file with a file that has no segments should fail validation")
		assert.Contains(t, err.Error(), "has no segments", "Error message should indicate no segments")
	})

	t.Run("non-existent nzb file", func(t *testing.T) {
		// Validate a non-existent file
		err := Validate("non-existent-file.nzb")
		assert.Error(t, err, "Validating a non-existent file should fail")
	})
}

func TestCompressWithZstd(t *testing.T) {
	segmentSize := uint64(1000)
	compressionConfig := config.NzbCompressionConfig{
		Enabled: true,
		Type:    config.CompressionTypeZstd,
		Level:   3,
	}

	generator := NewGenerator(segmentSize, compressionConfig, true).(*Generator)

	// Create test data
	testData := []byte("This is test data for compression")

	// Create a temporary file for the compressed output
	tempFile, err := os.CreateTemp("", "test-zstd-*.zst")
	require.NoError(t, err, "Failed to create temp file")
	defer func() {
		err := tempFile.Close()
		assert.NoError(t, err, "Failed to close temp file")
		err = os.Remove(tempFile.Name())
		assert.NoError(t, err, "Failed to remove temp file")
	}()

	// Compress the data
	err = generator.compressWithZstd(testData, tempFile.Name())
	require.NoError(t, err, "Failed to compress data with zstd")

	// Check that the compressed file exists and is not empty
	info, err := os.Stat(tempFile.Name())
	require.NoError(t, err, "Compressed file should exist")
	assert.Greater(t, info.Size(), int64(0), "Compressed file should not be empty")
}

func TestCompressWithBrotli(t *testing.T) {
	segmentSize := uint64(1000)
	compressionConfig := config.NzbCompressionConfig{
		Enabled: true,
		Type:    config.CompressionTypeBrotli,
		Level:   4,
	}

	generator := NewGenerator(segmentSize, compressionConfig, true).(*Generator)

	// Create test data
	testData := []byte("This is test data for compression")

	// Create a temporary file for the compressed output
	tempFile, err := os.CreateTemp("", "test-brotli-*.br")
	require.NoError(t, err, "Failed to create temp file")
	defer func() {
		err := tempFile.Close()
		assert.NoError(t, err, "Failed to close temp file")
		err = os.Remove(tempFile.Name())
		assert.NoError(t, err, "Failed to remove temp file")
	}()

	// Compress the data
	err = generator.compressWithBrotli(testData, tempFile.Name())
	require.NoError(t, err, "Failed to compress data with brotli")

	// Check that the compressed file exists and is not empty
	info, err := os.Stat(tempFile.Name())
	require.NoError(t, err, "Compressed file should exist")
	assert.Greater(t, info.Size(), int64(0), "Compressed file should not be empty")
}

func TestRemoveFile(t *testing.T) {
	gen := NewGenerator(1000, config.NzbCompressionConfig{}, false).(*Generator)
	gen.AddArticle(&article.Article{MessageID: "a1", OriginalName: "video.mkv", PartNumber: 1})
	gen.AddArticle(&article.Article{MessageID: "p1", OriginalName: "video.par2", PartNumber: 1})
	gen.AddArticle(&article.Article{MessageID: "p2", OriginalName: "video.par2", PartNumber: 2})
	gen.AddFileHash("video.par2", "abc")

	gen.RemoveFile("video.par2")

	if _, ok := gen.articles["video.par2"]; ok {
		t.Error("articles for removed file still present")
	}
	if _, ok := gen.articleIdx["video.par2"]; ok {
		t.Error("article index for removed file still present")
	}
	if _, ok := gen.filesHash["video.par2"]; ok {
		t.Error("hash for removed file still present")
	}
	if len(gen.articles["video.mkv"]) != 1 {
		t.Errorf("unrelated file lost articles: %d", len(gen.articles["video.mkv"]))
	}

	// Removing again, or a name never added, is a no-op.
	gen.RemoveFile("video.par2")
	gen.RemoveFile("never-added")
}
