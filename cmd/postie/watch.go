package main

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"syscall"

	"github.com/kipsilabs/postie/internal/config"
	"github.com/kipsilabs/postie/internal/database"
	"github.com/kipsilabs/postie/internal/pool"
	"github.com/kipsilabs/postie/internal/processor"
	"github.com/kipsilabs/postie/internal/queue"
	"github.com/kipsilabs/postie/internal/watcher"
	"github.com/spf13/cobra"
)

var watchCmd = &cobra.Command{
	Use:   "watch",
	Short: "Watch a directory for new files and upload them",
	Long: `Watch a directory for new files and automatically upload them when they meet the criteria.
The watch command will monitor the configured directory and upload files according to the settings in the configuration file.`,
	RunE: func(cmd *cobra.Command, args []string) error {
		ctx, cancel := context.WithCancel(cmd.Context())
		defer cancel()

		// Load configuration
		cfg, err := config.Load(configPath)
		if err != nil {
			slog.ErrorContext(ctx, "Error loading configuration", "error", err)
			return err
		}

		setupLogging(verbose)

		// Note: Postie instances are now created per-job within the processor

		// Get configurations
		watcherCfg := cfg.GetWatcherConfig()
		databaseCfg := cfg.GetDatabaseConfig()
		queueCfg := cfg.GetQueueConfig()

		// Set up directories
		watchDir := dirPath
		if watchDir == "" {
			watchDir = "./watch"
		}

		outputFolder := outputDir
		if outputFolder == "" {
			outputFolder = "./output"
		}

		// Ensure directories exist
		if _, err := os.Stat(watchDir); os.IsNotExist(err) {
			if err := os.MkdirAll(watchDir, 0755); err != nil {
				return fmt.Errorf("failed to create watch directory: %w", err)
			}
		} else if err != nil {
			return fmt.Errorf("failed to check watch directory: %w", err)
		}

		if _, err := os.Stat(outputFolder); os.IsNotExist(err) {
			if err := os.MkdirAll(outputFolder, 0755); err != nil {
				return fmt.Errorf("failed to create output directory: %w, %s", err, outputFolder)
			}
		} else if err != nil {
			return fmt.Errorf("failed to check output directory: %w", err)
		}

		// Initialize connection pool manager
		poolManager, err := pool.New(cfg)
		if err != nil {
			slog.ErrorContext(ctx, "Error creating connection pool manager", "error", err)
			return err
		}
		defer func() {
			if err := poolManager.Close(); err != nil {
				slog.ErrorContext(ctx, "Error closing connection pool manager", "error", err)
			}
		}()

		// Initialize database
		db, err := database.New(ctx, databaseCfg)
		if err != nil {
			slog.ErrorContext(ctx, "Error creating database", "error", err)
			return err
		}
		defer func() {
			if err := db.Close(); err != nil {
				slog.ErrorContext(ctx, "Error closing database", "error", err)
			}
		}()

		// Run database migrations
		if err := db.EnsureMigrationCompatibility(); err != nil {
			slog.ErrorContext(ctx, "Error running database migrations", "error", err)
			return err
		}

		// Initialize queue with database
		q, err := queue.New(ctx, db)
		if err != nil {
			slog.ErrorContext(ctx, "Error creating queue", "error", err)
			return err
		}
		defer func() {
			if err := q.Close(); err != nil {
				slog.ErrorContext(ctx, "Error closing queue", "error", err)
			}
		}()

		// Initialize processor
		proc := processor.New(processor.ProcessorOptions{
			Queue:                     q,
			Config:                    cfg,
			QueueConfig:               queueCfg,
			PoolManager:               poolManager,
			OutputFolder:              outputFolder,
			DeleteOriginalFile:        watcherCfg.DeleteOriginalFile,
			MaintainOriginalExtension: cfg.GetMaintainOriginalExtension(),
			WatchFolder:               watcherCfg.WatchDirectory,
			CanProcessNextItem:        nil, // CLI version doesn't need pending config management
		})

		// Start processor in background
		go func() {
			if err := proc.Start(ctx); err != nil && err != context.Canceled {
				slog.ErrorContext(ctx, "Processor error", "error", err)
			}
		}()

		// Create watcher (SingleNzbPerFolder is now in WatcherConfig)
		w := watcher.New(watcherCfg, q, proc, watchDir)

		// Handle shutdown signals
		sigChan := make(chan os.Signal, 1)
		signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)

		// Start watcher in a goroutine
		go func() {
			if err := w.Start(ctx); err != nil && err != context.Canceled {
				slog.ErrorContext(ctx, "Error running watcher", "error", err)
				cancel()
			}
		}()

		slog.Info("Watching directory", "dir", watchDir, "output", outputFolder)

		// Wait for shutdown signal
		<-sigChan
		slog.Info("Shutting down...")
		cancel()

		return nil
	},
}

func init() {
	rootCmd.AddCommand(watchCmd)
}
