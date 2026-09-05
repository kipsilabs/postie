package nzb

import (
	"archive/zip"
	"compress/flate"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/andybalholm/brotli"
	"github.com/javi11/nzbparser"
	"github.com/javi11/postie/internal/article"
	"github.com/javi11/postie/internal/config"
	"github.com/klauspost/compress/zstd"
)

// NZBGenerator defines the interface for generating NZB files
type NZBGenerator interface {
	// AddArticle adds an article to the generator
	AddArticle(article *article.Article)
	// AddFileHash adds a hash for a file
	AddFileHash(filename string, hash string)
	// RemoveFile drops every article (and hash) recorded for a file
	RemoveFile(filename string)
	// Generate creates an NZB file
	Generate(outputPath string) (string, error)
}

// Generator creates NZB files
type Generator struct {
	articles                  map[string][]*article.Article // filename -> articles
	articleIdx                map[string]map[string]int     // filename -> messageID -> index (for O(1) lookup)
	filesHash                 map[string]string             // filename -> checksums
	segmentSize               uint64                        // size of each segment in bytes
	compressionConfig         config.NzbCompressionConfig   // compression configuration
	maintainOriginalExtension bool                          // whether to maintain original file extension
	mx                        sync.RWMutex                  // mutex for concurrent access
}

// NewGenerator creates a new NZB generator
func NewGenerator(segmentSize uint64, compressionConfig config.NzbCompressionConfig, maintainOriginalExtension bool) NZBGenerator {
	return &Generator{
		articles:                  make(map[string][]*article.Article),
		articleIdx:                make(map[string]map[string]int),
		filesHash:                 make(map[string]string),
		segmentSize:               segmentSize,
		compressionConfig:         compressionConfig,
		maintainOriginalExtension: maintainOriginalExtension,
	}
}

// AddArticle adds an article to the generator
func (g *Generator) AddArticle(art *article.Article) {
	g.mx.Lock()
	defer g.mx.Unlock()
	filename := art.OriginalName

	// O(1) lookup for existing article by message ID
	if idx, ok := g.articleIdx[filename]; ok {
		if i, exists := idx[art.MessageID]; exists {
			// Replace the existing article
			g.articles[filename][i] = art
			return
		}
	}

	// Initialize index map for this filename if needed
	if g.articleIdx[filename] == nil {
		g.articleIdx[filename] = make(map[string]int)
	}

	// Add to index and append to articles
	g.articleIdx[filename][art.MessageID] = len(g.articles[filename])
	g.articles[filename] = append(g.articles[filename], art)
}

// RemoveFile drops every article and hash recorded for filename, so a file
// whose upload was abandoned part-way is not referenced by the NZB. Unknown
// names are a no-op.
func (g *Generator) RemoveFile(filename string) {
	g.mx.Lock()
	defer g.mx.Unlock()
	delete(g.articles, filename)
	delete(g.articleIdx, filename)
	delete(g.filesHash, filename)
}

// Generate creates an NZB file for all files
func (g *Generator) Generate(outputPath string) (string, error) {
	g.mx.RLock()
	defer g.mx.RUnlock()

	if len(g.articles) == 0 {
		return "", fmt.Errorf("no articles found")
	}

	// Generate the final NZB filename based on maintainOriginalExtension setting
	finalNzbPath := g.generateFinalNzbPath(outputPath)

	// Create NZB file
	nzbFile := &nzbparser.Nzb{
		Meta: map[string]string{
			"date":       time.Now().Format(time.RFC3339),
			"chunk_size": fmt.Sprintf("%d", g.segmentSize),
		},
	}

	// Add all files to NZB
	fileNumber := 0
	for filename, articles := range g.articles {
		if len(articles) == 0 {
			continue
		}

		// Sort articles by part number
		sort.Slice(articles, func(i, j int) bool {
			return articles[i].PartNumber < articles[j].PartNumber
		})

		// Calculate file size from all segments
		var fileSize int64
		for _, a := range articles {
			fileSize += int64(a.Size)
		}

		// Create file entry
		file := nzbparser.NzbFile{
			Subject:       articles[0].OriginalSubject,
			Groups:        articles[0].Groups,
			Poster:        articles[0].From,
			Date:          int(time.Now().Unix()),
			Bytes:         fileSize,
			Number:        articles[0].FileNumber,
			TotalSegments: len(articles),
			Filename:      articles[0].OriginalName,
		}

		// Add checksum if available
		if hash, ok := g.filesHash[filename]; ok {
			file.FileHash = hash
		}

		// Add segments
		for i, a := range articles {
			// Use configured segment size for all segments except the last one
			segmentSize := g.segmentSize
			if i == len(articles)-1 {
				segmentSize = a.Size
			}

			segment := nzbparser.NzbSegment{
				Bytes:  int(segmentSize),
				Number: a.PartNumber,
				ID:     a.MessageID,
			}
			file.Segments = append(file.Segments, segment)
		}

		// Add file to NZB
		nzbFile.Files = append(nzbFile.Files, file)
		fileNumber++
	}

	// Sort files
	sort.Slice(nzbFile.Files, func(i, j int) bool {
		return nzbFile.Files[i].Number < nzbFile.Files[j].Number
	})

	// Create output directory if it doesn't exist
	// Use filepath.Dir to get the parent directory, not the full path which includes the filename
	outputDir := filepath.Dir(outputPath)
	if _, err := os.Stat(outputDir); os.IsNotExist(err) {
		if err := os.MkdirAll(outputDir, 0755); err != nil {
			return "", fmt.Errorf("failed to create output directory: %w, %s", err, outputDir)
		}
	} else if err != nil {
		return "", fmt.Errorf("failed to check output directory: %w, %s", err, outputDir)
	}

	// Write NZB file
	data, err := nzbparser.Write(nzbFile)
	if err != nil {
		return "", fmt.Errorf("error writing NZB file: %w", err)
	}

	// Apply compression if enabled and write to final path
	if g.compressionConfig.Enabled {
		switch g.compressionConfig.Type {
		case config.CompressionTypeZstd:
			compressionPath := finalNzbPath + ".zst"
			if err := g.compressWithZstd(data, compressionPath); err != nil {
				return "", fmt.Errorf("error compressing NZB file with zstd: %w", err)
			}
			return compressionPath, nil
		case config.CompressionTypeBrotli:
			compressionPath := finalNzbPath + ".br"
			if err := g.compressWithBrotli(data, compressionPath); err != nil {
				return "", fmt.Errorf("error compressing NZB file with brotli: %w", err)
			}
			return compressionPath, nil
		case config.CompressionTypeZip:
			compressionPath := finalNzbPath + ".zip"
			if err := g.compressWithZip(data, compressionPath, finalNzbPath); err != nil {
				return "", fmt.Errorf("error compressing NZB file with zip: %w", err)
			}
			return compressionPath, nil
		default:
			// No compression or unknown type, write the file as is
			if err := os.WriteFile(finalNzbPath, data, 0644); err != nil {
				return "", fmt.Errorf("error writing NZB file: %w", err)
			}
			return finalNzbPath, nil
		}
	} else {
		// No compression, write the file as is
		if err := os.WriteFile(finalNzbPath, data, 0644); err != nil {
			return "", fmt.Errorf("error writing NZB file: %w", err)
		}
		return finalNzbPath, nil
	}
}

// compressWithZstd compresses data with zstd and writes it to the given path
func (g *Generator) compressWithZstd(data []byte, outputPath string) error {
	// Create the file
	f, err := os.Create(outputPath)
	if err != nil {
		return fmt.Errorf("error creating compressed file: %w", err)
	}
	defer func() {
		if err := f.Close(); err != nil {
			slog.Error("error closing file", "error", err)
		}
	}()

	// Create zstd encoder with the configured level
	level := zstd.EncoderLevel(g.compressionConfig.Level)
	enc, err := zstd.NewWriter(f, zstd.WithEncoderLevel(level))
	if err != nil {
		return fmt.Errorf("error creating zstd encoder: %w", err)
	}
	defer func() {
		if err := enc.Close(); err != nil {
			slog.Error("error closing zstd encoder", "error", err)
		}
	}()

	// Write compressed data
	if _, err := enc.Write(data); err != nil {
		return fmt.Errorf("error writing compressed data: %w", err)
	}

	return nil
}

// compressWithBrotli compresses data with brotli and writes it to the given path
func (g *Generator) compressWithBrotli(data []byte, outputPath string) error {
	// Create the file
	f, err := os.Create(outputPath)
	if err != nil {
		return fmt.Errorf("error creating compressed file: %w", err)
	}
	defer func() {
		if err := f.Close(); err != nil {
			slog.Error("error closing file", "error", err)
		}
	}()

	// Create brotli writer with the configured level
	w := brotli.NewWriterLevel(f, g.compressionConfig.Level)
	defer func() {
		if err := w.Close(); err != nil {
			slog.Error("error closing brotli writer", "error", err)
		}
	}()

	// Write compressed data
	if _, err := w.Write(data); err != nil {
		return fmt.Errorf("error writing compressed data: %w", err)
	}

	return nil
}

// compressWithZip compresses data with zip and writes it to the given path
func (g *Generator) compressWithZip(data []byte, outputPath string, originalNzbPath string) error {
	// Create the file
	f, err := os.Create(outputPath)
	if err != nil {
		return fmt.Errorf("error creating compressed file: %w", err)
	}
	defer func() {
		if err := f.Close(); err != nil {
			slog.Error("error closing file", "error", err)
		}
	}()

	// Create zip writer
	w := zip.NewWriter(f)
	defer func() {
		if err := w.Close(); err != nil {
			slog.Error("error closing zip writer", "error", err)
		}
	}()

	// Get the base filename for the entry in the zip
	nzbFilename := filepath.Base(originalNzbPath)

	// Create a file header with compression settings
	header := &zip.FileHeader{
		Name:   nzbFilename,
		Method: zip.Deflate,
	}

	// Set compression level by manipulating the extra field
	// This is a workaround since Go's zip package doesn't expose compression level directly
	w.RegisterCompressor(zip.Deflate, func(out io.Writer) (io.WriteCloser, error) {
		return flate.NewWriter(out, g.compressionConfig.Level)
	})

	zipFile, err := w.CreateHeader(header)
	if err != nil {
		return fmt.Errorf("error creating zip entry: %w", err)
	}

	// Write the NZB data to the zip file
	if _, err := zipFile.Write(data); err != nil {
		return fmt.Errorf("error writing data to zip: %w", err)
	}

	return nil
}

// AddFileHash adds a hash for a file
func (g *Generator) AddFileHash(filename string, hash string) {
	g.mx.Lock()
	defer g.mx.Unlock()

	g.filesHash[filename] = hash
}

// generateFinalNzbPath creates the final NZB path based on the configuration
func (g *Generator) generateFinalNzbPath(originalFilePath string) string {
	if strings.HasSuffix(strings.ToLower(originalFilePath), ".nzb") {
		return originalFilePath
	}

	dir := filepath.Dir(originalFilePath)
	basename := filepath.Base(originalFilePath)

	var filename string
	if g.maintainOriginalExtension {
		// Keep original extension: filename.ext.nzb
		filename = basename + ".nzb"
	} else {
		// Remove original extension: filename.nzb
		ext := filepath.Ext(basename)
		nameWithoutExt := strings.TrimSuffix(basename, ext)
		filename = nameWithoutExt + ".nzb"
	}

	return filepath.Join(dir, filename)
}

// Parse reads an NZB file
func Parse(path string) (*nzbparser.Nzb, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("error reading NZB file: %w", err)
	}
	return nzbparser.ParseString(string(data))
}

// Validate checks if an NZB file is valid
func Validate(path string) error {
	nzbFile, err := Parse(path)
	if err != nil {
		return fmt.Errorf("error parsing NZB file: %w", err)
	}

	if len(nzbFile.Files) == 0 {
		return fmt.Errorf("NZB file contains no files")
	}

	for _, file := range nzbFile.Files {
		if len(file.Segments) == 0 {
			return fmt.Errorf("file %s has no segments", file.Subject)
		}
	}

	return nil
}
