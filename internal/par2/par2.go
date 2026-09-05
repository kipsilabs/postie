package par2

import (
	"context"
	"fmt"
	"log/slog"
	"math"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"

	"github.com/google/uuid"
	"github.com/javi11/par2go"

	"github.com/javi11/postie/internal/config"
	"github.com/javi11/postie/internal/progress"
	"github.com/javi11/postie/pkg/fileinfo"
)

var parregexp = regexp.MustCompile(`(?i)(\.vol\d+\+(\d+))?\.par2$`)

// maxPar2Blocks is the PAR2 specification limit for the maximum number of
// data + recovery blocks. The format uses 16-bit identifiers, so the
// theoretical maximum is 2^15 = 32768.
const maxPar2Blocks = 32768

// Par2Executor defines the interface for executing par2 commands.
type Par2Executor interface {
	Create(ctx context.Context, files []fileinfo.FileInfo) ([]string, error)
	CreateInDirectory(ctx context.Context, files []fileinfo.FileInfo, outputDir string) ([]string, error)
	// CreateSet bundles all input files into a single par2 set named setName.
	// folderDir is the on-disk root of the folder being posted (e.g.
	// "<watchRoot>/ShowS01"). Each FileDesc packet records the path of the
	// file relative to folderDir (e.g. "extras/bonus.mkv") so downloaders
	// such as SABnzbd can recreate the folder tree inside the job directory.
	CreateSet(ctx context.Context, files []fileinfo.FileInfo, outputDir, setName, folderDir string) ([]string, error)
}

// NativeExecutor implements Par2Executor using the built-in Go PAR2 creator.
type NativeExecutor struct {
	articleSize uint64
	cfg         *config.Par2Config
	jobProgress progress.JobProgress
}

// New creates a new NativeExecutor.
func New(articleSize uint64, cfg *config.Par2Config, jobProgress progress.JobProgress) *NativeExecutor {
	return &NativeExecutor{
		articleSize: articleSize,
		cfg:         cfg,
		jobProgress: jobProgress,
	}
}

// checkExistingPar2Files checks if PAR2 files already exist for the given source file.
// It always checks the source directory first, then falls back to TempDir if configured.
func (p *NativeExecutor) checkExistingPar2Files(ctx context.Context, sourceFile fileinfo.FileInfo) ([]string, bool) {
	// Always check the source directory first — this is where pre-existing PAR2 files live.
	sourceDirPath := filepath.Dir(sourceFile.Path)
	if existing, ok := checkExistingPar2FilesInPath(ctx, sourceFile, sourceDirPath); ok {
		return existing, ok
	}

	// Fall back to TempDir if configured (reuse from a previous generation run).
	if p.cfg.TempDir != "" && p.cfg.TempDir != sourceDirPath {
		return checkExistingPar2FilesInPath(ctx, sourceFile, p.cfg.TempDir)
	}

	return nil, false
}

// checkExistingPar2FilesInDir checks if PAR2 files already exist in a specific directory.
func (p *NativeExecutor) checkExistingPar2FilesInDir(ctx context.Context, sourceFile fileinfo.FileInfo, dirPath string) ([]string, bool) {
	return checkExistingPar2FilesInPath(ctx, sourceFile, dirPath)
}

// checkExistingPar2FilesInPath is the shared implementation for checking existing PAR2 files.
func checkExistingPar2FilesInPath(ctx context.Context, sourceFile fileinfo.FileInfo, dirPath string) ([]string, bool) {
	baseName := filepath.Base(sourceFile.Path)
	par2FileName := baseName + ".par2"
	mainPar2Path := filepath.Join(dirPath, par2FileName)

	// Check if main PAR2 file exists
	if _, err := os.Stat(mainPar2Path); os.IsNotExist(err) {
		return nil, false
	}

	// Collect all existing PAR2 files (main + volume files)
	var existingPaths []string
	existingPaths = append(existingPaths, mainPar2Path)

	// Find all volume files
	entries, err := os.ReadDir(dirPath)
	if err != nil {
		slog.WarnContext(ctx, "Failed to read directory for existing par2 volumes", "error", err)
		return nil, false
	}

	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if strings.HasPrefix(name, baseName) && strings.Contains(name, ".vol") && strings.HasSuffix(name, ".par2") {
			existingPaths = append(existingPaths, filepath.Join(dirPath, name))
		}
	}

	slog.InfoContext(ctx, "Found existing PAR2 files, skipping generation",
		"sourceFile", sourceFile.Path, "par2Files", len(existingPaths))

	return existingPaths, true
}

// Create creates PAR2 parity files for the given input files.
func (p *NativeExecutor) Create(ctx context.Context, files []fileinfo.FileInfo) ([]string, error) {
	slog.InfoContext(ctx, "Starting par2 creation process", "executor", "NativeExecutor")

	var createdPar2Paths []string
	for _, file := range files {
		if filepath.Ext(file.Path) == ".par2" {
			continue
		}

		// Check if PAR2 files already exist for this file
		if existingPaths, exists := p.checkExistingPar2Files(ctx, file); exists {
			createdPar2Paths = append(createdPar2Paths, existingPaths...)
			continue
		}

		// Determine output directory
		var dirPath string
		if p.cfg.TempDir != "" {
			dirPath = p.cfg.TempDir
			if err := os.MkdirAll(dirPath, 0755); err != nil {
				slog.ErrorContext(ctx, "Failed to create temp directory", "path", dirPath, "error", err)
				dirPath = filepath.Dir(file.Path)
			}
		} else {
			dirPath = filepath.Dir(file.Path)
		}

		paths, err := p.createPar2ForFile(ctx, file, dirPath)
		if err != nil {
			return nil, err
		}
		createdPar2Paths = append(createdPar2Paths, paths...)
	}

	return createdPar2Paths, nil
}

// CreateInDirectory creates PAR2 files with optional output directory specification.
func (p *NativeExecutor) CreateInDirectory(ctx context.Context, files []fileinfo.FileInfo, outputDir string) ([]string, error) {
	slog.InfoContext(ctx, "Starting par2 creation process", "executor", "NativeExecutor", "outputDir", outputDir)

	var createdPar2Paths []string
	for _, file := range files {
		if filepath.Ext(file.Path) == ".par2" {
			continue
		}

		// Determine output directory based on parameters and configuration
		var dirPath string
		if outputDir != "" {
			dirPath = outputDir
			if err := os.MkdirAll(dirPath, 0755); err != nil {
				slog.ErrorContext(ctx, "Failed to create output directory", "path", dirPath, "error", err)
				dirPath = filepath.Dir(file.Path)
			} else {
				// Check source directory first — pre-existing PAR2 files take priority.
				if existingPaths, exists := checkExistingPar2FilesInPath(ctx, file, filepath.Dir(file.Path)); exists {
					createdPar2Paths = append(createdPar2Paths, existingPaths...)
					continue
				}
				if existingPaths, exists := p.checkExistingPar2FilesInDir(ctx, file, dirPath); exists {
					createdPar2Paths = append(createdPar2Paths, existingPaths...)
					continue
				}
			}
		} else if p.cfg.TempDir != "" {
			dirPath = p.cfg.TempDir
			if err := os.MkdirAll(dirPath, 0755); err != nil {
				slog.ErrorContext(ctx, "Failed to create temp directory", "path", dirPath, "error", err)
				dirPath = filepath.Dir(file.Path)
			} else {
				if existingPaths, exists := p.checkExistingPar2Files(ctx, file); exists {
					createdPar2Paths = append(createdPar2Paths, existingPaths...)
					continue
				}
			}
		} else {
			dirPath = filepath.Dir(file.Path)
			if existingPaths, exists := p.checkExistingPar2Files(ctx, file); exists {
				createdPar2Paths = append(createdPar2Paths, existingPaths...)
				continue
			}
		}

		paths, err := p.createPar2ForFile(ctx, file, dirPath)
		if err != nil {
			return nil, err
		}
		createdPar2Paths = append(createdPar2Paths, paths...)
	}

	return createdPar2Paths, nil
}

// CreateSet bundles all input files into a single par2 set named setName.
// folderDir is the on-disk root of the folder being posted.  Each FileDesc
// packet records filepath.Rel(folderDir, file.Path) so SABnzbd / NZBGet
// can recreate the exact folder tree inside the job directory on disk.
func (p *NativeExecutor) CreateSet(ctx context.Context, files []fileinfo.FileInfo, outputDir, setName, folderDir string) ([]string, error) {
	if len(files) == 0 {
		return nil, fmt.Errorf("par2: no input files for set %q", setName)
	}
	if setName == "" {
		return nil, fmt.Errorf("par2: empty set name")
	}

	// Filter out any par2 files defensively — callers should not include them.
	inputs := make([]fileinfo.FileInfo, 0, len(files))
	for _, f := range files {
		if filepath.Ext(f.Path) == ".par2" {
			continue
		}
		inputs = append(inputs, f)
	}
	if len(inputs) == 0 {
		return nil, fmt.Errorf("par2: no non-par2 input files for set %q", setName)
	}

	dirPath := outputDir
	if dirPath == "" {
		if p.cfg.TempDir != "" {
			dirPath = p.cfg.TempDir
		} else {
			dirPath = filepath.Dir(inputs[0].Path)
		}
	}
	if err := os.MkdirAll(dirPath, 0755); err != nil {
		return nil, fmt.Errorf("par2: create output dir %s: %w", dirPath, err)
	}

	// Reuse existing set if already on disk.
	if existing, ok := checkExistingPar2SetInPath(ctx, setName, dirPath); ok {
		return existing, nil
	}

	// Slice size: smallest size that yields ≤ maxPar2Blocks slices across all
	// inputs while respecting SliceSize config and SIMD alignment.
	totalSize := uint64(0)
	maxFileSize := uint64(0)
	for _, f := range inputs {
		totalSize += f.Size
		if f.Size > maxFileSize {
			maxFileSize = f.Size
		}
	}
	parBlockSize := p.computeSetBlockSize(totalSize, maxFileSize)
	if parBlockSize < 128 {
		slog.WarnContext(ctx, "Block size too small for SIMD-safe PAR2 set creation, skipping",
			"setName", setName, "totalSize", totalSize, "blockSize", parBlockSize)
		return nil, nil
	}

	// Total input slices and recovery blocks
	totalSlices := 0
	for _, f := range inputs {
		n := int(math.Ceil(float64(f.Size) / float64(parBlockSize)))
		if n == 0 {
			n = 1
		}
		totalSlices += n
	}
	redundancyPct := parseRedundancyPercentage(p.cfg.Redundancy, totalSize, parBlockSize)
	numRecovery := max(int(math.Ceil(float64(totalSlices)*redundancyPct/100.0)), 1)

	par2Path := filepath.Join(dirPath, setName+".par2")

	progressID := uuid.New()
	progressName := fmt.Sprintf("PAR2: %s", setName)
	var pg progress.Progress
	if p.jobProgress != nil {
		pg = p.jobProgress.AddProgress(progressID, progressName, progress.ProgressTypePar2Generation, 100)
	}

	par2Inputs := make([]par2go.InputFile, len(inputs))
	for i, f := range inputs {
		// Compute the path relative to folderDir (e.g. "extras/bonus.mkv").
		// SABnzbd already creates a job folder named after the NZB title, so
		// FileDesc must NOT include the top-level folder name as a prefix.
		name, relErr := filepath.Rel(folderDir, f.Path)
		if relErr != nil || name == "" || name == "." {
			name = filepath.Base(f.Path)
		}
		// Forward slashes only — par2go validates this and downloaders rely on it.
		name = filepath.ToSlash(name)
		par2Inputs[i] = par2go.InputFile{Path: f.Path, Name: name}
	}

	opts := par2go.Options{
		SliceSize:     int(parBlockSize),
		NumRecovery:   numRecovery,
		NumGoroutines: p.cfg.NumGoroutines,
		MemoryLimit:   p.cfg.MemoryLimit,
		Creator:       "Postie",
		Logger:        slog.Default(),
		OnProgress: func(phase string, pct float64) {
			if pg == nil {
				return
			}
			var overallPct float64
			switch phase {
			case "hashing":
				overallPct = pct * 20
			case "encoding":
				overallPct = 20 + pct*75
			case "writing":
				overallPct = 95 + pct*5
			}
			delta := int64(overallPct) - pg.GetCurrent()
			if delta > 0 {
				pg.UpdateProgress(delta)
			}
		},
	}

	slog.InfoContext(ctx, "Creating PAR2 set",
		"setName", setName,
		"files", len(par2Inputs),
		"blockSize", parBlockSize,
		"inputSlices", totalSlices,
		"recoveryBlocks", numRecovery,
		"redundancy", redundancyPct)

	if err := par2go.CreateWithNames(ctx, par2Path, par2Inputs, opts); err != nil {
		if ctx.Err() == context.Canceled {
			slog.InfoContext(ctx, "Par2 set creation cancelled", "setName", setName)
			return nil, ctx.Err()
		}
		return nil, fmt.Errorf("failed to create par2 set %s: %w", setName, err)
	}

	if p.jobProgress != nil {
		p.jobProgress.FinishProgress(progressID)
	}

	return collectPar2SetFiles(ctx, dirPath, setName, par2Path), nil
}

// computeSetBlockSize picks a slice size for a multi-file par2 set such that
// the total slice count stays under maxPar2Blocks and SIMD alignment is
// preserved. Honors p.cfg.SliceSize when sane.
func (p *NativeExecutor) computeSetBlockSize(totalSize, maxFileSize uint64) uint64 {
	var parBlockSize uint64
	if p.cfg.SliceSize > 0 && uint64(p.cfg.SliceSize) <= maxFileSize {
		parBlockSize = uint64(p.cfg.SliceSize)
	} else {
		parBlockSize = calculateParBlockSize(totalSize, p.articleSize)
	}
	// Ensure block size yields ≤ maxPar2Blocks total slices.
	if parBlockSize > 0 {
		approxSlices := totalSize / parBlockSize
		if approxSlices >= maxPar2Blocks {
			parBlockSize = (totalSize / (maxPar2Blocks - 1)) + 1
		}
	}
	// SIMD-safe alignment when a single file is smaller than the block size.
	const simdSafeAlignment = uint64(128)
	if maxFileSize > 0 && parBlockSize > maxFileSize {
		if maxFileSize < simdSafeAlignment {
			return 0
		}
		parBlockSize = (maxFileSize / simdSafeAlignment) * simdSafeAlignment
	}
	// Every slice size handed to par2go must be SIMD-aligned. The ParPar C
	// backend reads in vector lanes up to 64 bytes wide; a non-aligned size
	// causes overreads past the slice buffer and intermittent segfaults on
	// Linux x86_64 / AVX-512. 128 covers all current strides.
	return alignDown(parBlockSize, simdSafeAlignment)
}

// checkExistingPar2SetInPath looks for an already-generated par2 set in
// dirPath named "<setName>.par2" plus any companion volume files.
func checkExistingPar2SetInPath(ctx context.Context, setName, dirPath string) ([]string, bool) {
	main := filepath.Join(dirPath, setName+".par2")
	if _, err := os.Stat(main); os.IsNotExist(err) {
		return nil, false
	}
	paths := collectPar2SetFiles(ctx, dirPath, setName, main)
	slog.InfoContext(ctx, "Found existing PAR2 set, skipping generation",
		"setName", setName, "par2Files", len(paths))
	return paths, true
}

// collectPar2SetFiles returns the main par2 path plus all companion volume
// files matching "<setName>.vol*.par2" in dirPath.
func collectPar2SetFiles(ctx context.Context, dirPath, setName, mainPath string) []string {
	out := []string{mainPath}
	entries, err := os.ReadDir(dirPath)
	if err != nil {
		slog.WarnContext(ctx, "Failed to read directory for par2 volumes", "error", err)
		return out
	}
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if strings.HasPrefix(name, setName) && strings.Contains(name, ".vol") && strings.HasSuffix(name, ".par2") {
			out = append(out, filepath.Join(dirPath, name))
		}
	}
	return out
}

// computeFileBlockSize picks a SIMD-safe slice size for a single file. Returns
// 0 when no safe block size exists (file smaller than the SIMD alignment).
// Exposed at package level so tests can assert the invariant directly.
func computeFileBlockSize(fileSize, articleSize uint64, configuredSliceSize int64) uint64 {
	const simdSafeAlignment = uint64(128)

	var parBlockSize uint64
	if configuredSliceSize > 0 && uint64(configuredSliceSize) <= fileSize {
		parBlockSize = uint64(configuredSliceSize)
	} else {
		// Configured SliceSize exceeds file size (or is unset): fall back to
		// article-size-based calculation. Clamping SliceSize to ≈file.Size
		// creates only 1-2 slices with a nearly-zero last slice, which triggers
		// undefined behavior in the ParPar C SIMD backend on Linux x86_64 (AVX-512).
		parBlockSize = calculateParBlockSize(fileSize, articleSize)
	}
	if fileSize > 0 && parBlockSize > fileSize {
		if fileSize < simdSafeAlignment {
			return 0
		}
		parBlockSize = (fileSize / simdSafeAlignment) * simdSafeAlignment
	}
	parBlockSize = alignDown(parBlockSize, simdSafeAlignment)
	if parBlockSize < simdSafeAlignment {
		return 0
	}
	return parBlockSize
}

// createPar2ForFile creates PAR2 files for a single input file in the given directory.
func (p *NativeExecutor) createPar2ForFile(ctx context.Context, file fileinfo.FileInfo, dirPath string) ([]string, error) {
	// Slice size handed to par2go MUST be a multiple of 128 (SIMD-safe). Otherwise
	// the ParPar C backend overreads past the slice on the last vector lane and
	// randomly segfaults when the over-read crosses an unmapped page (AVX-512 on
	// Linux x86_64). See computeFileBlockSize for the alignment policy.
	parBlockSize := computeFileBlockSize(file.Size, p.articleSize, p.cfg.SliceSize)
	if parBlockSize == 0 {
		slog.WarnContext(ctx, "Block size too small for SIMD-safe PAR2 creation, skipping",
			"path", file.Path, "size", file.Size)
		return nil, nil
	}

	par2FileName := filepath.Base(file.Path) + ".par2"
	par2Path := filepath.Join(dirPath, par2FileName)

	// Parse redundancy to determine number of recovery blocks
	redundancyPct := parseRedundancyPercentage(p.cfg.Redundancy, file.Size, parBlockSize)
	numInputSlices := int(math.Ceil(float64(file.Size) / float64(parBlockSize)))
	if numInputSlices == 0 {
		numInputSlices = 1
	}
	numRecovery := max(int(math.Ceil(float64(numInputSlices)*redundancyPct/100.0)), 1)

	slog.DebugContext(ctx, "PAR2 creation parameters",
		"file", file.Path,
		"blockSize", parBlockSize,
		"inputSlices", numInputSlices,
		"recoveryBlocks", numRecovery,
		"redundancy", redundancyPct)

	// Initialize progress tracking
	progressID := uuid.New()
	progressName := fmt.Sprintf("PAR2: %s", filepath.Base(file.Path))
	var pg progress.Progress
	if p.jobProgress != nil {
		pg = p.jobProgress.AddProgress(progressID, progressName, progress.ProgressTypePar2Generation, 100)
	}

	opts := par2go.Options{
		SliceSize:     int(parBlockSize),
		NumRecovery:   numRecovery,
		NumGoroutines: p.cfg.NumGoroutines,
		MemoryLimit:   p.cfg.MemoryLimit,
		Creator:       "Postie",
		Logger:        slog.Default(),
		OnProgress: func(phase string, pct float64) {
			if pg == nil {
				return
			}
			// Map phases to progress: hashing=0-20%, encoding=20-95%, writing=95-100%
			var overallPct float64
			switch phase {
			case "hashing":
				overallPct = pct * 20
			case "encoding":
				overallPct = 20 + pct*75
			case "writing":
				overallPct = 95 + pct*5
			}
			delta := int64(overallPct) - pg.GetCurrent()
			if delta > 0 {
				pg.UpdateProgress(delta)
			}
		},
	}

	err := par2go.Create(ctx, par2Path, []string{file.Path}, opts)
	if err != nil {
		if ctx.Err() == context.Canceled {
			slog.InfoContext(ctx, "Par2 creation cancelled", "path", file.Path)
			return nil, ctx.Err()
		}
		return nil, fmt.Errorf("failed to create par2 for %s: %w", file.Path, err)
	}

	if p.jobProgress != nil {
		p.jobProgress.FinishProgress(progressID)
	}

	slog.InfoContext(ctx, "Par2 creation completed successfully", "file", file.Path)

	// Collect all created PAR2 files (main + volumes)
	var createdPaths []string
	createdPaths = append(createdPaths, par2Path)

	baseName := filepath.Base(file.Path)
	entries, err := os.ReadDir(dirPath)
	if err != nil {
		slog.WarnContext(ctx, "Failed to read directory to find par2 volumes", "error", err)
		return createdPaths, nil
	}

	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if strings.HasPrefix(name, baseName) && strings.Contains(name, ".vol") && strings.HasSuffix(name, ".par2") {
			createdPaths = append(createdPaths, filepath.Join(dirPath, name))
		}
	}

	return createdPaths, nil
}

// parseRedundancyPercentage parses the redundancy config string into a percentage.
// Supports formats: "10", "10%", "1n*1.2" (ParPar formula).
func parseRedundancyPercentage(redundancy string, fileSize uint64, blockSize uint64) float64 {
	redundancy = strings.TrimSpace(redundancy)

	// Try ParPar formula: "Xn*Y" where X is multiplier and Y is factor
	if strings.Contains(redundancy, "n") {
		// Parse "1n*1.2" style formulas
		// This means: ceil(inputSlices * factor) recovery blocks
		// Convert to percentage
		parts := strings.SplitN(redundancy, "*", 2)
		if len(parts) == 2 {
			factor, err := strconv.ParseFloat(parts[1], 64)
			if err == nil {
				// Convert to percentage: factor * 100 gives us the percentage
				// "1n*1.2" means 120% recovery blocks relative to input
				nParts := strings.SplitN(parts[0], "n", 2)
				multiplier := 1.0
				if nParts[0] != "" {
					if m, err := strconv.ParseFloat(nParts[0], 64); err == nil {
						multiplier = m
					}
				}
				return multiplier * factor * 100
			}
		}
	}

	// Try percentage: "10%" or "10"
	cleaned := strings.TrimSuffix(redundancy, "%")
	if pct, err := strconv.ParseFloat(cleaned, 64); err == nil {
		return pct
	}

	// Default: 10%
	slog.Warn("Could not parse redundancy, defaulting to 10%", "redundancy", redundancy)
	return 10.0
}

// IsPar2File returns true if the given path matches a PAR2 file pattern.
func IsPar2File(path string) bool {
	return parregexp.MatchString(path)
}

// NewExecutor returns the appropriate Par2Executor based on config.
// If cfg.ParparBinaryPath is non-empty, returns a BinaryExecutor; otherwise NativeExecutor.
func NewExecutor(articleSize uint64, cfg *config.Par2Config, jobProgress progress.JobProgress) Par2Executor {
	if cfg != nil && cfg.ParparBinaryPath != "" {
		return NewBinaryExecutor(articleSize, cfg, jobProgress)
	}
	return New(articleSize, cfg, jobProgress)
}

// alignDown rounds v down to the nearest multiple of alignment.
func alignDown(v, alignment uint64) uint64 {
	return (v / alignment) * alignment
}

// calculateParBlockSize calculates the appropriate PAR2 block size for the given file.
// The returned value is always a multiple of 4 as required by the PAR2 specification.
func calculateParBlockSize(fileSize uint64, articleSize uint64) uint64 {
	var blockSize uint64
	if fileSize/articleSize < maxPar2Blocks {
		blockSize = articleSize
	} else {
		minParBlockSize := (fileSize / maxPar2Blocks) + 1
		multiplier := minParBlockSize / articleSize
		if minParBlockSize%articleSize != 0 {
			multiplier++
		}
		blockSize = multiplier * articleSize
	}
	return alignDown(blockSize, 4)
}
