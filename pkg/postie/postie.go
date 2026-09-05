package postie

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"github.com/javi11/postie/internal/config"
	"github.com/javi11/postie/internal/nzb"
	"github.com/javi11/postie/internal/par2"
	"github.com/javi11/postie/internal/pool"
	"github.com/javi11/postie/internal/poster"
	"github.com/javi11/postie/internal/progress"
	"github.com/javi11/postie/internal/transferwriter"
	"github.com/javi11/postie/pkg/fileinfo"
	"golang.org/x/sync/errgroup"
)

type Postie struct {
	par2Cfg                   *config.Par2Config
	postingCfg                config.PostingConfig
	par2runner                par2.Par2Executor
	poster                    poster.Poster
	compressionCfg            config.NzbCompressionConfig
	postUploadScriptCfg       config.PostUploadScriptConfig
	maintainOriginalExtension bool
	postCheckCfg              config.PostCheck
	jobProgress               progress.JobProgress
	queue                     QueueInterface
	// recorder is the per-job durable manifest recorder, or nil in standalone
	// mode. When set, the poster writes manifests during upload and Postie marks
	// the transfer's files uploaded (for the durable verification service) once
	// posting completes.
	recorder *transferwriter.Recorder
	// deleteOriginal records whether this job's originals should be deleted
	// after successful verification (persisted into the transfer's cleanup
	// policy at completion). Set by the caller before Post.
	deleteOriginal bool
}

// SetDeleteOriginal records whether the job's original files should be deleted
// after the durable verification service confirms the upload. Must be called
// before Post. No effect in standalone mode (deletion is handled inline there).
func (p *Postie) SetDeleteOriginal(v bool) {
	p.deleteOriginal = v
}

// QueueInterface defines the queue methods needed by Postie
type QueueInterface interface {
	UpdateScriptStatus(ctx context.Context, itemID string, status string, retryCount int, lastError string, nextRetryAt *time.Time, firstFailureAt *time.Time) error
	MarkScriptCompleted(ctx context.Context, itemID string) error
}

// New creates a Postie that owns a private transfer runtime. It is retained as
// a compatibility entry point for the CLI and external package users; the
// processor path uses NewWithRuntime to share one process-wide runtime across
// all jobs.
func New(
	ctx context.Context,
	cfg config.Config,
	poolManager *pool.Manager,
	jobProgress progress.JobProgress,
	queue QueueInterface,
) (*Postie, error) {
	return NewWithRuntime(ctx, cfg, poolManager, jobProgress, queue, nil, "")
}

// NewWithRuntime creates a Postie that borrows shared resources from rt. When
// rt is nil a private PAR2 scheduler is created honouring the configured
// par2.max_concurrent_jobs, preserving standalone behaviour. transferID binds
// this job's durable manifests; when empty (or rt has no store) manifest
// recording is disabled.
func NewWithRuntime(
	ctx context.Context,
	cfg config.Config,
	poolManager *pool.Manager,
	jobProgress progress.JobProgress,
	queue QueueInterface,
	rt *Runtime,
	transferID string,
) (*Postie, error) {
	// Get PAR2 configuration
	par2Cfg, err := cfg.GetPar2Config(ctx)
	if err != nil {
		return nil, err
	}

	postingConfig := cfg.GetPostingConfig()
	compressionConfig := cfg.GetNzbCompressionConfig()
	postUploadScriptConfig := cfg.GetPostUploadScriptConfig()
	maintainOriginalExtension := cfg.GetMaintainOriginalExtension()

	// Create the per-job PAR2 executor (carries this job's progress + config),
	// then gate its execution through the shared, process-wide PAR2 scheduler so
	// the configured memory limit applies per active job rather than per queue
	// job. Falls back to a private scheduler when no runtime is supplied.
	par2Executor := par2.NewExecutor(postingConfig.ArticleSizeInBytes, par2Cfg, jobProgress)
	par2Scheduler := rt.Par2Scheduler()
	if par2Scheduler == nil {
		maxJobs := 1
		if par2Cfg != nil && par2Cfg.MaxConcurrentJobs > 0 {
			maxJobs = par2Cfg.MaxConcurrentJobs
		}
		par2Scheduler = par2.NewScheduler(maxJobs)
	}
	par2runner := par2.NewScheduledExecutor(par2Executor, par2Scheduler)

	// Build the per-job durable manifest recorder (nil in standalone mode). It
	// doubles as the poster's manifest sink; pass an untyped-nil sink when
	// absent to avoid a non-nil interface wrapping a nil pointer.
	recorder := rt.NewManifestRecorder(transferID)
	var sink poster.ManifestSink
	if recorder != nil {
		sink = recorder
	}

	// Create poster with progress manager, sharing the process-wide upload
	// engine (worker + buffer-budget limits) and the per-job manifest sink.
	p, err := poster.NewWithEngine(ctx, cfg, poolManager, jobProgress, rt.UploadEngine(), sink)
	if err != nil {
		slog.ErrorContext(ctx, "Error creating poster", "error", err)

		return nil, err
	}

	return &Postie{
		par2Cfg:                   par2Cfg,
		par2runner:                par2runner,
		poster:                    p,
		postingCfg:                postingConfig,
		compressionCfg:            compressionConfig,
		postUploadScriptCfg:       postUploadScriptConfig,
		maintainOriginalExtension: maintainOriginalExtension,
		postCheckCfg:              cfg.GetPostCheckConfig(),
		jobProgress:               jobProgress,
		queue:                     queue,
		recorder:                  recorder,
	}, nil
}

// removePar2AfterPost reports whether generated PAR2 files should be deleted as
// soon as posting succeeds. Only in standalone mode with maintain_par2_files
// off: in durable mode the background verification may still need to re-post
// PAR2 articles from those files, and the transfer cleaner removes them once
// the transfer is verified.
func (p *Postie) removePar2AfterPost() bool {
	if p.recorder != nil {
		return false
	}
	return p.par2Cfg.MaintainPar2Files == nil || !*p.par2Cfg.MaintainPar2Files
}

// completeTransferUpload marks the transfer's files uploaded so the durable
// verification service can verify them, scheduling the first check after the
// configured propagation delay. No-op in standalone mode (no recorder).
func (p *Postie) completeTransferUpload(ctx context.Context) {
	if p.recorder == nil {
		return
	}
	delay := p.postCheckCfg.RetryDelay.ToDuration()
	now := time.Now().UTC()
	if err := p.recorder.CompleteUpload(ctx, now, now.Add(delay), p.deleteOriginal); err != nil {
		slog.WarnContext(ctx, "Failed to mark transfer uploaded for verification", "error", err)
	}
}

func (p *Postie) Close() {
	p.poster.Close()
	if p.jobProgress != nil {
		p.jobProgress.Close()
	}
}

// CleanupPar2Files removes PAR2 files for the given source file
// This method can be called when a job fails permanently to clean up orphaned PAR2 files
func (p *Postie) CleanupPar2Files(ctx context.Context, sourceFile fileinfo.FileInfo) {
	var dirPath string
	if p.par2Cfg != nil && p.par2Cfg.TempDir != "" {
		dirPath = p.par2Cfg.TempDir
	} else {
		dirPath = filepath.Dir(sourceFile.Path)
	}

	baseName := filepath.Base(sourceFile.Path)
	par2FileName := baseName + ".par2"
	mainPar2Path := filepath.Join(dirPath, par2FileName)

	// Remove main PAR2 file
	if _, err := os.Stat(mainPar2Path); err == nil {
		safeRemoveFile(ctx, mainPar2Path)
		slog.DebugContext(ctx, "Cleaned up main PAR2 file", "file", mainPar2Path)
	}

	// Find and remove all volume files
	entries, err := os.ReadDir(dirPath)
	if err != nil {
		slog.WarnContext(ctx, "Failed to read directory for PAR2 cleanup", "error", err)
		return
	}

	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		// Match patterns like .vol0+1.par2, .vol1+1.par2, etc.
		if strings.HasPrefix(name, baseName) && strings.Contains(name, ".vol") && strings.HasSuffix(name, ".par2") {
			volumePath := filepath.Join(dirPath, name)
			safeRemoveFile(ctx, volumePath)
			slog.DebugContext(ctx, "Cleaned up PAR2 volume file", "file", volumePath)
		}
	}

	slog.InfoContext(ctx, "PAR2 cleanup completed", "sourceFile", sourceFile.Path)
}

// safeRemoveFile attempts to remove a file with retry logic
func safeRemoveFile(ctx context.Context, filePath string) {
	maxRetries := 3
	baseDelay := 100 * time.Millisecond

	for i := range maxRetries {
		if err := os.Remove(filePath); err == nil {
			return // Success
		}

		// On Windows, files might still be locked. Wait and retry.
		if i < maxRetries-1 {
			delay := baseDelay * time.Duration(i+1)
			select {
			case <-ctx.Done():
				slog.ErrorContext(ctx, "File cleanup cancelled", "file", filePath)
				return
			case <-time.After(delay):
				// Continue to next retry
			}
		}
	}

	// Final attempt if error just ignore it is a tmp file it will be deleted automatically
	_ = os.Remove(filePath)
}

func (p *Postie) Post(ctx context.Context, files []fileinfo.FileInfo, rootDir string, outputDir string, forceFolderMode bool) (nzbPath string, retErr error) {
	if len(files) == 0 {
		return "", fmt.Errorf("no files to post")
	}

	// On a successful upload, mark the transfer's files uploaded so the durable
	// verification service picks them up and the queue slot can be released.
	// In durable mode the in-poster checkLoop is skipped, so a nil error here
	// means posting succeeded with nothing left to verify inline. No-op in
	// standalone mode (no recorder).
	defer func() {
		if retErr == nil {
			p.completeTransferUpload(ctx)
		}
	}()

	// Use folder mode (single NZB) if explicitly requested via forceFolderMode
	// This is set to true for:
	// - Add Folder button (explicit folder upload)
	// - Watch mode with SingleNzbPerFolder enabled (FOLDER: prefix in queue)
	if forceFolderMode {
		// Post all files as a single unit (folder mode)
		return p.postFolder(ctx, files, rootDir, outputDir)
	}

	// Start posting (one NZB per file - traditional mode)
	startTime := time.Now()
	var lastNzbPath string
	var lastDeferredErr *poster.DeferredCheckError

	for _, f := range files {
		slog.InfoContext(ctx, "Posting file", "file", f.Path)

		var nzbPath string
		var err error
		if *p.postingCfg.WaitForPar2 {
			nzbPath, err = p.post(ctx, f, rootDir, outputDir)
		} else {
			nzbPath, err = p.postInParallel(ctx, f, rootDir, outputDir)
		}

		if err != nil {
			var de *poster.DeferredCheckError
			if errors.As(err, &de) {
				// Non-fatal - NZB was generated, collect deferred articles
				lastDeferredErr = de
			} else {
				return "", err
			}
		}
		lastNzbPath = nzbPath
	}

	// Print final statistics
	stats := p.poster.Stats()
	elapsed := time.Since(startTime)

	slog.InfoContext(ctx, "Upload completed in", "elapsed", elapsed.Round(time.Second))
	slog.InfoContext(ctx, "Articles posted", "count", stats.ArticlesPosted)
	slog.InfoContext(ctx, "Articles checked", "count", stats.ArticlesChecked)
	slog.InfoContext(ctx, "Total bytes", "count", stats.BytesPosted)
	slog.InfoContext(ctx, "Errors", "count", stats.ArticleErrors)

	// Return deferred check error if present (non-fatal, NZBs were generated)
	if lastDeferredErr != nil {
		return lastNzbPath, lastDeferredErr
	}
	return lastNzbPath, nil
}

func (p *Postie) postInParallel(
	ctx context.Context,
	f fileinfo.FileInfo,
	rootDir string,
	outputDir string,
) (string, error) {
	var (
		createdPar2Paths []string
		err              error
		postingSucceeded bool
	)
	defer func() {
		// Only process PAR2 files if posting was successful
		if !postingSucceeded {
			// Keep PAR2 files on failure for retry attempts
			return
		}

		if p.removePar2AfterPost() {
			for _, path := range createdPar2Paths {
				safeRemoveFile(ctx, path)
			}
		}
	}()

	nzbGen := nzb.NewGenerator(p.postingCfg.ArticleSizeInBytes, p.compressionCfg, p.maintainOriginalExtension)

	errg := errgroup.Group{}

	errg.Go(func() error {
		// Determine PAR2 output directory based on maintain_par2_files setting
		var par2OutputDir string
		if p.par2Cfg.MaintainPar2Files != nil && *p.par2Cfg.MaintainPar2Files {
			// Generate PAR2 files directly in output directory
			relativePath := relativePathFrom(rootDir, f.Path)
			par2OutputDir = filepath.Join(outputDir, relativePath)

			slog.DebugContext(ctx, "Generating PAR2 files directly in output directory",
				"sourceFile", f.Path, "outputDir", par2OutputDir)
		}
		// If par2OutputDir is empty, CreateInDirectory will use default behavior (temp/source dir)

		createdPar2Paths, err = p.par2runner.CreateInDirectory(ctx, []fileinfo.FileInfo{f}, par2OutputDir)
		if err != nil {
			if !errors.Is(err, context.Canceled) {
				slog.ErrorContext(ctx, "Error during par2 creation. Upload will continue without par2.", "error", err)
			}

			return nil
		}

		if err := p.poster.Post(ctx, createdPar2Paths, rootDir, nzbGen); err != nil {
			if !errors.Is(err, context.Canceled) {
				slog.ErrorContext(ctx, fmt.Sprintf("Error during upload of par2 files: %s. Upload will continue without par2.", createdPar2Paths), "error", err)
			}

			return nil
		}

		return nil
	})

	var deferredErr *poster.DeferredCheckError

	errg.Go(func() error {
		if err := p.poster.Post(ctx, []string{f.Path}, rootDir, nzbGen); err != nil {
			// Check if this is a non-fatal deferred check error
			var de *poster.DeferredCheckError
			if errors.As(err, &de) {
				deferredErr = de
				return nil // Non-fatal, continue to NZB generation
			}
			if !errors.Is(err, context.Canceled) {
				slog.ErrorContext(ctx, fmt.Sprintf("Error during upload: %s", f.Path), "error", err)
			}

			return err
		}

		return nil
	})

	if err := errg.Wait(); err != nil {
		return "", err
	}

	// Generate single NZB file for all files
	relativePath := relativePathFrom(rootDir, f.Path)

	// Use the original filename as input for NZB generation
	nzbPath := filepath.Join(outputDir, relativePath, filepath.Base(f.Path))
	finalPath, err := nzbGen.Generate(nzbPath)
	if err != nil {
		return "", fmt.Errorf("error generating NZB file: %w", err)
	}

	// Mark posting as successful so PAR2 files get cleaned up
	postingSucceeded = true

	// Return deferred check error if present (non-fatal, NZB was generated)
	if deferredErr != nil {
		return finalPath, deferredErr
	}
	return finalPath, nil
}

func (p *Postie) post(
	ctx context.Context,
	f fileinfo.FileInfo,
	rootDir string,
	outputDir string,
) (string, error) {
	var (
		createdPar2Paths []string
		err              error
		postingSucceeded bool
	)

	defer func() {
		// Only process PAR2 files if posting was successful
		if !postingSucceeded {
			// Keep PAR2 files on failure for retry attempts
			return
		}

		if p.removePar2AfterPost() {
			for _, path := range createdPar2Paths {
				safeRemoveFile(ctx, path)
			}
		}
	}()

	filesPath := []string{f.Path}
	nzbGen := nzb.NewGenerator(p.postingCfg.ArticleSizeInBytes, p.compressionCfg, p.maintainOriginalExtension)

	if *p.par2Cfg.Enabled {
		// Determine PAR2 output directory based on maintain_par2_files setting
		var par2OutputDir string
		if p.par2Cfg.MaintainPar2Files != nil && *p.par2Cfg.MaintainPar2Files {
			// Generate PAR2 files directly in output directory
			relativePath := relativePathFrom(rootDir, f.Path)
			par2OutputDir = filepath.Join(outputDir, relativePath)

			slog.DebugContext(ctx, "Generating PAR2 files directly in output directory",
				"sourceFile", f.Path, "outputDir", par2OutputDir)
		}
		// If par2OutputDir is empty, CreateInDirectory will use default behavior (temp/source dir)

		createdPar2Paths, err = p.par2runner.CreateInDirectory(ctx, []fileinfo.FileInfo{f}, par2OutputDir)
		if err != nil {
			if !errors.Is(err, context.Canceled) {
				slog.ErrorContext(ctx, "Error during par2 creation. Upload will continue without par2.", "error", err)
			}

			return "", err
		}

		filesPath = append(filesPath, createdPar2Paths...)
	}

	var deferredErr *poster.DeferredCheckError
	if err := p.poster.Post(ctx, filesPath, rootDir, nzbGen); err != nil {
		// Check if this is a non-fatal deferred check error
		if errors.As(err, &deferredErr) {
			slog.InfoContext(ctx, "Some articles deferred for later verification", "file", f.Path, "deferred", len(deferredErr.FailedArticles))
		} else {
			if !errors.Is(err, context.Canceled) {
				slog.ErrorContext(ctx, fmt.Sprintf("Error during upload: %s", filesPath), "error", err)
			}
			return "", err
		}
	}

	// Generate single NZB file for all files
	relativePath := relativePathFrom(rootDir, f.Path)

	// Use the original filename as input for NZB generation
	nzbPath := filepath.Join(outputDir, relativePath, filepath.Base(f.Path))
	finalPath, err := nzbGen.Generate(nzbPath)
	if err != nil {
		return "", fmt.Errorf("error generating NZB file: %w", err)
	}

	// Mark posting as successful so PAR2 files get cleaned up
	postingSucceeded = true

	// Return deferred check error if present (non-fatal, NZB was generated)
	if deferredErr != nil {
		return finalPath, deferredErr
	}
	return finalPath, nil
}

// hasPar2Files returns true if any file in the list is a PAR2 file.
func hasPar2Files(files []fileinfo.FileInfo) bool {
	for _, f := range files {
		if par2.IsPar2File(f.Path) {
			return true
		}
	}
	return false
}

// postFolder posts all files from a folder as a single NZB
func (p *Postie) postFolder(ctx context.Context, files []fileinfo.FileInfo, rootDir string, outputDir string) (string, error) {
	if len(files) == 0 {
		return "", fmt.Errorf("no files to post in folder")
	}

	startTime := time.Now()

	folderName := deriveFolderName(rootDir, files)
	// All generated files (NZB and PAR2) go into a dedicated subfolder named after
	// the source folder, regardless of whether watch and output are on the same volume.
	folderOutputDir := filepath.Join(outputDir, folderName)

	slog.InfoContext(ctx, "Posting folder as single NZB", "folder", folderName, "files", len(files))

	var (
		createdPar2Paths []string
		err              error
		postingSucceeded bool
	)

	defer func() {
		// Only process PAR2 files if posting was successful
		if !postingSucceeded {
			// Keep PAR2 files on failure for retry attempts
			return
		}

		if p.removePar2AfterPost() {
			for _, path := range createdPar2Paths {
				safeRemoveFile(ctx, path)
			}
		}
	}()

	// Create a single NZB generator for all files
	nzbGen := nzb.NewGenerator(p.postingCfg.ArticleSizeInBytes, p.compressionCfg, p.maintainOriginalExtension)

	// Collect all file paths and build relative paths map for subject generation
	var allFilePaths []string
	relativePaths := make(map[string]string)
	for _, f := range files {
		allFilePaths = append(allFilePaths, f.Path)
		// Use RelativePath for subject if available, otherwise use filename
		if f.RelativePath != "" {
			relativePaths[f.Path] = f.RelativePath
		}
	}

	if *p.postingCfg.WaitForPar2 {
		// Create PAR2 files for all files in the folder
		if *p.par2Cfg.Enabled {
			// Skip PAR2 generation if option enabled and PAR2 files already exist in source
			skipGeneration := p.par2Cfg.SkipIfPar2Exists != nil &&
				*p.par2Cfg.SkipIfPar2Exists && hasPar2Files(files)
			if skipGeneration {
				slog.InfoContext(ctx, "Skipping PAR2 generation: existing PAR2 files detected in source folder",
					"folder", folderName)
			} else {
				// Determine PAR2 output directory based on maintain_par2_files setting
				var par2OutputDir string
				if p.par2Cfg.MaintainPar2Files != nil && *p.par2Cfg.MaintainPar2Files {
					// For folder posting, PAR2 files go into the folder-specific output subdirectory
					par2OutputDir = folderOutputDir

					slog.DebugContext(ctx, "Generating PAR2 files directly in output directory",
						"folder", folderName, "outputDir", par2OutputDir)
				}
				// If par2OutputDir is empty, CreateSet will use default behavior (temp/source dir)
				// folderDir is the on-disk root of the folder; FileDesc paths are computed
				// relative to it so SABnzbd recreates the tree inside the job folder.
				folderDir := filepath.Join(rootDir, folderName)
				createdPar2Paths, err = p.par2runner.CreateSet(ctx, files, par2OutputDir, folderName, folderDir)
				if err != nil {
					if !errors.Is(err, context.Canceled) {
						slog.ErrorContext(ctx, "Error during par2 creation. Upload will continue without par2.", "error", err)
					}
					// Continue without PAR2 files
				} else {
					allFilePaths = append(allFilePaths, createdPar2Paths...)
					// par2 set files live at the folder root — basenames in NZB subjects.
				}
			}
		}

		var deferredErr *poster.DeferredCheckError
		// Post all files (including PAR2) together with relative paths for subjects
		if err := p.poster.PostWithRelativePaths(ctx, allFilePaths, rootDir, nzbGen, relativePaths); err != nil {
			if errors.As(err, &deferredErr) {
				slog.InfoContext(ctx, "Some articles deferred for later verification", "folder", folderName, "deferred", len(deferredErr.FailedArticles))
			} else {
				if !errors.Is(err, context.Canceled) {
					slog.ErrorContext(ctx, "Error during folder upload", "folder", folderName, "error", err)
				}
				return "", err
			}
		}

		// Generate NZB and return with deferred error if present
		nzbPath := filepath.Join(folderOutputDir, folderName+".nzb")
		finalPath, nzbErr := nzbGen.Generate(nzbPath)
		if nzbErr != nil {
			return "", fmt.Errorf("error generating NZB file for folder: %w", nzbErr)
		}
		postingSucceeded = true

		if deferredErr != nil {
			return finalPath, deferredErr
		}
		return finalPath, nil
	}

	// Post files and PAR2 in parallel
	var deferredErr *poster.DeferredCheckError
	{
		errg := errgroup.Group{}

		// Create PAR2 files in parallel
		errg.Go(func() error {
			if !*p.par2Cfg.Enabled {
				return nil
			}

			// Skip PAR2 generation if option enabled and PAR2 files already exist in source
			if p.par2Cfg.SkipIfPar2Exists != nil && *p.par2Cfg.SkipIfPar2Exists && hasPar2Files(files) {
				slog.InfoContext(ctx, "Skipping PAR2 generation: existing PAR2 files detected in source folder",
					"folder", folderName)
				return nil
			}

			// Determine PAR2 output directory based on maintain_par2_files setting
			var par2OutputDir string
			if p.par2Cfg.MaintainPar2Files != nil && *p.par2Cfg.MaintainPar2Files {
				// For folder posting, PAR2 files go into the folder-specific output subdirectory
				par2OutputDir = folderOutputDir

				slog.DebugContext(ctx, "Generating PAR2 files directly in output directory",
					"folder", folderName, "outputDir", par2OutputDir)
			}

			folderDir := filepath.Join(rootDir, folderName)
			createdPar2Paths, err = p.par2runner.CreateSet(ctx, files, par2OutputDir, folderName, folderDir)
			if err != nil {
				if !errors.Is(err, context.Canceled) {
					slog.ErrorContext(ctx, "Error during par2 creation. Upload will continue without par2.", "error", err)
				}
				return nil
			}

			// par2 set files live at the folder root — basenames in NZB subjects.
			par2RelPaths := map[string]string{}
			if err := p.poster.PostWithRelativePaths(ctx, createdPar2Paths, rootDir, nzbGen, par2RelPaths); err != nil {
				if !errors.Is(err, context.Canceled) {
					slog.ErrorContext(ctx, "Error during upload of par2 files. Upload will continue without par2.", "error", err)
				}
				return nil
			}
			return nil
		})

		// Post main files with relative paths for subjects
		errg.Go(func() error {
			if err := p.poster.PostWithRelativePaths(ctx, allFilePaths, rootDir, nzbGen, relativePaths); err != nil {
				// Check if this is a non-fatal deferred check error
				var de *poster.DeferredCheckError
				if errors.As(err, &de) {
					deferredErr = de
					return nil // Non-fatal, continue to NZB generation
				}
				if !errors.Is(err, context.Canceled) {
					slog.ErrorContext(ctx, "Error during folder upload", "folder", folderName, "error", err)
				}
				return err
			}
			return nil
		})

		if err := errg.Wait(); err != nil {
			return "", err
		}
	}

	// Generate single NZB file for the entire folder
	// Use folder name as the base for NZB filename, placed inside the folder-specific output subdir
	nzbPath := filepath.Join(folderOutputDir, folderName+".nzb")
	finalPath, err := nzbGen.Generate(nzbPath)
	if err != nil {
		return "", fmt.Errorf("error generating NZB file for folder: %w", err)
	}

	// Mark posting as successful so PAR2 files get cleaned up
	postingSucceeded = true

	// Print final statistics
	stats := p.poster.Stats()
	elapsed := time.Since(startTime)

	slog.InfoContext(ctx, "Folder upload completed", "folder", folderName, "elapsed", elapsed.Round(time.Second))
	slog.InfoContext(ctx, "Articles posted", "count", stats.ArticlesPosted)
	slog.InfoContext(ctx, "Articles checked", "count", stats.ArticlesChecked)
	slog.InfoContext(ctx, "Total bytes", "count", stats.BytesPosted)
	slog.InfoContext(ctx, "Errors", "count", stats.ArticleErrors)

	// Return deferred check error if present (non-fatal, NZB was generated)
	if deferredErr != nil {
		return finalPath, deferredErr
	}
	return finalPath, nil
}

// ExecutePostUploadScript executes the post-upload script for a completed item
// This should be called after the file has been marked as completed in the database
func (p *Postie) ExecutePostUploadScript(ctx context.Context, nzbPath string, sourcePath string, itemID string) error {
	return runPostUploadScript(ctx, p.postUploadScriptCfg, p.queue, nzbPath, sourcePath, itemID)
}

// runPostUploadScript runs the configured post-upload script and tracks its
// retry status in the queue. Extracted from ExecutePostUploadScript so the
// durable verification cleanup path can run the script (after verification) too,
// not just the upload path. A nil queue skips status tracking; a disabled or
// empty command is a no-op.
func runPostUploadScript(ctx context.Context, cfg config.PostUploadScriptConfig, q QueueInterface, nzbPath, sourcePath, itemID string) error {
	if !cfg.Enabled || cfg.Command == "" {
		return nil
	}

	slog.InfoContext(ctx, "Executing post upload script", "command", cfg.Command, "nzb_path", nzbPath, "source_path", sourcePath, "item_id", itemID)

	timeoutCtx, cancel := context.WithTimeout(ctx, cfg.Timeout.ToDuration())
	defer cancel()

	command := strings.ReplaceAll(cfg.Command, "{nzb_path}", nzbPath)
	command = strings.ReplaceAll(command, "{source_path}", sourcePath)
	command = strings.ReplaceAll(command, "{source_dir}", filepath.Dir(sourcePath))

	var cmd *exec.Cmd
	if runtime.GOOS == "windows" {
		cmd = exec.CommandContext(timeoutCtx, "cmd", "/C", command)
	} else {
		cmd = exec.CommandContext(timeoutCtx, "sh", "-c", command)
	}
	cmd.Dir = filepath.Dir(nzbPath)

	output, err := cmd.CombinedOutput()
	if err != nil {
		errorMsg := fmt.Sprintf("script failed: %v, output: %s", err, string(output))
		slog.ErrorContext(ctx, "Error executing post upload script", "error", err, "output", string(output), "command", command)

		if q != nil {
			baseDelay := cfg.RetryDelay.ToDuration()
			now := time.Now()
			nextRetry := now.Add(baseDelay)
			if updateErr := q.UpdateScriptStatus(ctx, itemID, "pending_retry", 0, errorMsg, &nextRetry, &now); updateErr != nil {
				slog.ErrorContext(ctx, "Failed to track script failure", "error", updateErr)
			}
		}

		return fmt.Errorf("post upload script failed: %w", err)
	}

	if q != nil {
		if updateErr := q.MarkScriptCompleted(ctx, itemID); updateErr != nil {
			slog.ErrorContext(ctx, "Failed to mark script as completed", "error", updateErr)
		}
	}

	slog.InfoContext(ctx, "Post upload script executed successfully", "command", command, "output", string(output))
	return nil
}

// buildPar2RelativePaths builds a relative-path map for PAR2 files by matching each
// par2 file's base name to its source file's base name (par2 names are sourceBase+".par2"
// or sourceBase+".vol*+*.par2"). The relative path is derived from the source file's
// relative path directory joined with the par2 base name.
func buildPar2RelativePaths(sourceFiles []fileinfo.FileInfo, par2Paths []string) map[string]string {
	result := make(map[string]string)
	for _, par2Path := range par2Paths {
		par2Base := filepath.Base(par2Path)
		for _, sf := range sourceFiles {
			if sf.RelativePath == "" {
				continue
			}
			sfBase := filepath.Base(sf.Path)
			if strings.HasPrefix(par2Base, sfBase+".") {
				result[par2Path] = filepath.Join(filepath.Dir(sf.RelativePath), par2Base)
				break
			}
		}
	}
	return result
}

// deriveFolderName determines the top-level subfolder name from files relative to rootDir.
// When rootDir is the watch folder and files live under a subfolder (e.g. SingleNzbPerFolder
// watcher mode or the "Add Folder" button), this returns that subfolder name rather than the
// watch folder's own name.
// Falls back to filepath.Base(rootDir) when files are directly in rootDir or when no files
// are provided. Falls back further to "upload" when rootDir is "/" or otherwise yields an
// empty/ambiguous name.
func deriveFolderName(rootDir string, files []fileinfo.FileInfo) string {
	if len(files) > 0 {
		relPath, err := filepath.Rel(rootDir, files[0].Path)
		if err == nil {
			parts := strings.SplitN(filepath.ToSlash(relPath), "/", 2)
			if len(parts) > 1 && parts[0] != "." && parts[0] != "" {
				return parts[0]
			}
		}
	}
	// Fallback: use the base name of rootDir itself
	name := filepath.Base(rootDir)
	if name == "." || name == "/" || name == "" {
		return "upload"
	}
	return name
}

// relativePathFrom computes the relative path of filePath's directory from rootDir.
// Falls back to empty string (placing output directly in outputDir) if paths
// cannot be made relative (e.g. cross-volume on Windows).
func relativePathFrom(rootDir, filePath string) string {
	dirPath := filepath.Dir(filePath)
	rel, err := filepath.Rel(rootDir, dirPath)
	if err != nil || rel == "." {
		return ""
	}
	return rel
}
