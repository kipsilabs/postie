package main

import (
	"context"
	"embed"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"time"

	"github.com/kipsilabs/postie/internal/backend"
	"github.com/wailsapp/wails/v2"
	"github.com/wailsapp/wails/v2/pkg/logger"
	"github.com/wailsapp/wails/v2/pkg/menu"
	"github.com/wailsapp/wails/v2/pkg/menu/keys"
	"github.com/wailsapp/wails/v2/pkg/options"
	"github.com/wailsapp/wails/v2/pkg/options/assetserver"
	"github.com/wailsapp/wails/v2/pkg/options/mac"
	wailsruntime "github.com/wailsapp/wails/v2/pkg/runtime"
)

//go:embed all:frontend/build
var assets embed.FS

//go:embed build/appicon.png
var icon []byte

// getMainCrashLogPath returns a writable path for crash logs
// It tries temp directory first, then falls back to current directory
func getMainCrashLogPath() string {
	// Try to get app paths first (may fail early in startup)
	if appPaths, err := backend.GetAppPaths(); err == nil && appPaths.Data != "" {
		crashPath := filepath.Join(appPaths.Data, "postie_crash.log")
		// Verify the directory exists or can be created
		crashDir := filepath.Dir(crashPath)
		if err := os.MkdirAll(crashDir, 0755); err == nil {
			return crashPath
		}
	}

	// Try temp directory as fallback
	tempDir := os.TempDir()
	tempPath := filepath.Join(tempDir, "postie", "postie_crash.log")
	if err := os.MkdirAll(filepath.Dir(tempPath), 0755); err == nil {
		return tempPath
	}

	// Last resort: current directory
	return "postie_crash.log"
}

// recoverMainPanic handles panic recovery at the main function level
func recoverMainPanic() {
	if r := recover(); r != nil {
		stack := debug.Stack()
		slog.Error("Critical panic in main application",
			"panic", r,
			"stack", string(stack))

		// Create detailed crash log for debugging, especially on Windows
		crashLogPath := getMainCrashLogPath()
		if crashFile, err := os.OpenFile(crashLogPath, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644); err == nil {
			_, _ = fmt.Fprintf(crashFile, "=== POSTIE CRASH REPORT ===\n")
			_, _ = fmt.Fprintf(crashFile, "Time: %s\n", time.Now().Format(time.RFC3339))
			_, _ = fmt.Fprintf(crashFile, "OS: %s\n", runtime.GOOS)
			_, _ = fmt.Fprintf(crashFile, "Arch: %s\n", runtime.GOARCH)
			_, _ = fmt.Fprintf(crashFile, "Go Version: %s\n", runtime.Version())
			_, _ = fmt.Fprintf(crashFile, "Panic: %v\n\n", r)
			_, _ = fmt.Fprintf(crashFile, "Stack trace:\n%s\n", string(stack))
			_, _ = fmt.Fprintf(crashFile, "=== END CRASH REPORT ===\n\n")
			_ = crashFile.Close()

			fmt.Printf("Critical error: %v\n", r)
			fmt.Printf("Detailed crash log written to: %s\n", crashLogPath)
		} else {
			fmt.Printf("Critical error: %v (could not write crash log: %v)\n", r, err)
		}

		os.Exit(1)
	}
}

func main() {
	// Set up panic recovery for the entire application
	defer recoverMainPanic()
	// Create an instance of the app structure
	app := backend.NewApp()

	// Variable to store the context from OnStartup
	var appCtx context.Context

	// Create the menu
	appMenu := menu.NewMenu()

	// File menu
	fileMenu := appMenu.AddSubmenu("File")
	fileMenu.AddText("Settings", keys.Control("comma"), func(_ *menu.CallbackData) {
		app.NavigateToSettings()
	})
	fileMenu.AddSeparator()
	fileMenu.AddText("Quit", keys.Control("q"), func(_ *menu.CallbackData) {
		wailsruntime.Quit(context.Background())
	})

	// Edit menu
	editMenu := appMenu.AddSubmenu("Edit")
	editMenu.AddText("Undo", keys.CmdOrCtrl("z"), func(_ *menu.CallbackData) {
		if appCtx != nil {
			wailsruntime.EventsEmit(appCtx, "menu-undo")
		}
	})
	editMenu.AddText("Redo", keys.CmdOrCtrl("y"), func(_ *menu.CallbackData) {
		if appCtx != nil {
			wailsruntime.EventsEmit(appCtx, "menu-redo")
		}
	})
	editMenu.AddSeparator()
	editMenu.AddText("Cut", keys.CmdOrCtrl("x"), func(_ *menu.CallbackData) {
		if appCtx != nil {
			wailsruntime.EventsEmit(appCtx, "menu-cut")
		}
	})
	editMenu.AddText("Copy", keys.CmdOrCtrl("c"), func(_ *menu.CallbackData) {
		if appCtx != nil {
			wailsruntime.EventsEmit(appCtx, "menu-copy")
		}
	})
	editMenu.AddText("Paste", keys.CmdOrCtrl("v"), func(_ *menu.CallbackData) {
		if appCtx != nil {
			wailsruntime.EventsEmit(appCtx, "menu-paste")
		}
	})
	editMenu.AddSeparator()
	editMenu.AddText("Select All", keys.CmdOrCtrl("a"), func(_ *menu.CallbackData) {
		if appCtx != nil {
			wailsruntime.EventsEmit(appCtx, "menu-select-all")
		}
	})

	// View menu
	viewMenu := appMenu.AddSubmenu("View")
	viewMenu.AddText("Dashboard", keys.Control("1"), func(_ *menu.CallbackData) {
		app.NavigateToDashboard()
	})
	viewMenu.AddText("Settings", keys.Control("2"), func(_ *menu.CallbackData) {
		app.NavigateToSettings()
	})

	// Create application with options
	err := wails.Run(&options.App{
		Title:  "Postie UI",
		Width:  1024,
		Height: 768,
		AssetServer: &assetserver.Options{
			Assets: assets,
		},
		BackgroundColour: &options.RGBA{R: 27, G: 38, B: 54, A: 1},
		DisableResize:    false,
		SingleInstanceLock: &options.SingleInstanceLock{
			UniqueId: "postie",
		},
		EnableDefaultContextMenu: true,
		Menu:                     appMenu,
		DragAndDrop: &options.DragAndDrop{
			EnableFileDrop: true,
			// Windows has no native drop handler: the only path to wails:file-drop is the
			// webview's JS drop event posting the files back to Go. Disabling the webview
			// drop there calls AllowExternalDrag(false) and kills that path entirely.
			DisableWebViewDrop: runtime.GOOS != "windows",
			CSSDropProperty:    "--wails-drop-target",
			CSSDropValue:       "drop",
		},
		Logger: logger.NewDefaultLogger(),
		OnStartup: func(ctx context.Context) {
			appCtx = ctx
			app.Startup(ctx)

			// Set up file drop handler with crash protection
			wailsruntime.OnFileDrop(ctx, func(x, y int, paths []string) {
				// Wrap file drop handling in its own panic recovery
				defer func() {
					if r := recover(); r != nil {
						stack := debug.Stack()
						slog.Error("Panic in file drop handler",
							"panic", r,
							"paths", paths,
							"stack", string(stack))

						// Emit error event to frontend
						wailsruntime.EventsEmit(ctx, "file-drop-error", fmt.Sprintf("Critical error: %v", r))

						// Write to crash log for debugging using consistent path
						crashLogPath := getMainCrashLogPath()
						if crashFile, err := os.OpenFile(crashLogPath, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644); err == nil {
							_, _ = fmt.Fprintf(crashFile, "=== FILE DROP PANIC ===\n")
							_, _ = fmt.Fprintf(crashFile, "Time: %s\n", time.Now().Format(time.RFC3339))
							_, _ = fmt.Fprintf(crashFile, "Paths: %v\n", paths)
							_, _ = fmt.Fprintf(crashFile, "Panic: %v\n", r)
							_, _ = fmt.Fprintf(crashFile, "Stack:\n%s\n", string(stack))
							_, _ = fmt.Fprintf(crashFile, "=== END FILE DROP PANIC ===\n\n")
							_ = crashFile.Close()
						}
					}
				}()

				if err := app.HandleDroppedFiles(paths); err != nil {
					slog.Error("Error handling dropped files", "error", err, "paths", paths)
					// Emit error event to frontend for user notification
					wailsruntime.EventsEmit(ctx, "file-drop-error", err.Error())
				} else {
					// Emit success event to frontend
					wailsruntime.EventsEmit(ctx, "file-drop-success", len(paths))
				}
			})
		},
		OnShutdown: func(ctx context.Context) {
			// Gracefully shutdown the app and cleanup resources
			app.Shutdown()
		},
		LogLevelProduction: logger.DEBUG,
		Bind: []any{
			app,
		},
		Mac: &mac.Options{
			About: &mac.AboutInfo{
				Title:   "Postie",
				Message: "© 2025 Postie",
				Icon:    icon,
			},
			Appearance:          mac.NSAppearanceNameAccessibilityHighContrastVibrantDark,
			WindowIsTranslucent: true,
		},
	})

	if err != nil {
		fmt.Printf("Error starting GUI: %v\n", err)
		os.Exit(1)
	}
}
