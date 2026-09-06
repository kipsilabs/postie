<script lang="ts">
import { goto } from "$app/navigation";
import apiClient from "$lib/api/client";
import DashboardHeader from "$lib/components/dashboard/DashboardHeader.svelte";
import ProgressSection from "$lib/components/dashboard/ProgressSection.svelte";
import ProviderStatus from "$lib/components/dashboard/ProviderStatus.svelte";
import QueueSection from "$lib/components/dashboard/QueueSection.svelte";
import QueueStats from "$lib/components/dashboard/QueueStats.svelte";
import WatcherStatus from "$lib/components/dashboard/WatcherStatus.svelte";
import { t } from "$lib/i18n";
import { appStatus, runningJobs } from "$lib/stores/app";
import { toastStore } from "$lib/stores/toast";
import { uploadActions } from "$lib/stores/upload";
import { Plus } from "lucide-svelte";
import { onDestroy, onMount } from "svelte";

let needsConfiguration = false;
let criticalConfigError = false;
let isDragOver = false;
let dragCounter = 0;
let fileDropOff: (() => void) | null = null;
let destroyed = false;

onMount(() => {
	// Set up drag over detection for UI feedback
	window.addEventListener("dragenter", handleDragEnter);
	window.addEventListener("dragleave", handleDragLeave);
	window.addEventListener("dragover", handleDragOver);
	window.addEventListener("drop", handleDrop);

	registerWailsFileDrop();

	// Listen for file drop events from backend
	apiClient.on("file-drop-success", () => {
		// Hide overlay when files are successfully processed
		isDragOver = false;
		dragCounter = 0;
	});

	apiClient.on("file-drop-error", (error) => {
		const errMessage = error as string;
		// Hide overlay when there's an error
		isDragOver = false;
		dragCounter = 0;
		toastStore.error($t("common.common.error"), errMessage);
	});

	apiClient.on("upload-complete", (data) => {
		console.log("Upload complete:", data);
		// Mark all processing files as completed when upload is complete
		uploadActions.completeUpload();
	});

	apiClient.on("upload-error", (error) => {
		const errMessage = error as string;
		console.log("Upload error:", errMessage);
		// Handle upload errors from server
		toastStore.error($t("common.common.error"), errMessage || "Upload failed");
	});

	// Listen for job permanent failure events
	apiClient.on("job-error", (data) => {
		const { fileName, error } = data as { fileName: string; error: string };
		console.log("Job failed permanently:", fileName, error);
		toastStore.error($t("dashboard.job_failed", { values: { fileName } }), error);
	});

	// Listen for job status events - the ProgressSection component now fetches progress directly
	apiClient.on("queue-updated", () => {
		// Refresh progress when queue is updated
		console.log("Queue updated, progress will be refreshed by ProgressSection");
	});

	// Subscribe to app status
	const unsubscribe = appStatus.subscribe((status) => {
		needsConfiguration = status.needsConfiguration;
		criticalConfigError = status.criticalConfigError;

		// Redirect to setup wizard if this is first start
		if (status.isFirstStart) {
			goto("/setup");
			return;
		}

		// Redirect to settings if configuration is needed or there's a critical error
		if (needsConfiguration || criticalConfigError) {
			goto("/settings");
		}
	});

	return unsubscribe;
});

onDestroy(() => {
	// Clean up drag event listeners
	window.removeEventListener("dragenter", handleDragEnter);
	window.removeEventListener("dragleave", handleDragLeave);
	window.removeEventListener("dragover", handleDragOver);
	window.removeEventListener("drop", handleDrop);

	destroyed = true;
	fileDropOff?.();
	fileDropOff = null;
});

// On Windows the Go-side OnFileDrop handler only ever fires if the JS runtime
// installs its own drop listener, which is what posts the dropped files back to
// the webview. Registering here is what makes drag & drop work on Windows.
async function registerWailsFileDrop() {
	if (apiClient.environment !== "wails") {
		return;
	}

	try {
		const { OnFileDrop, OnFileDropOff } = await import(
			"$lib/wailsjs/runtime/runtime"
		);

		if (destroyed) {
			return;
		}

		// useDropTarget: false — the default re-checks elementFromPoint against
		// --wails-drop-target, which our conditionally rendered overlay breaks.
		OnFileDrop(() => {}, false);
		fileDropOff = OnFileDropOff;
	} catch (error) {
		console.error("Failed to register Wails file drop handler:", error);
	}
}

function handleDragEnter(e: DragEvent) {
	e.preventDefault();
	if (e.dataTransfer?.types.includes("Files")) {
		dragCounter++;
		isDragOver = true;
	}
}

function handleDragLeave(e: DragEvent) {
	e.preventDefault();
	if (e.dataTransfer?.types.includes("Files")) {
		dragCounter--;
		if (dragCounter <= 0) {
			dragCounter = 0;
			isDragOver = false;
		}
	}
}

function handleDragOver(e: DragEvent) {
	e.preventDefault();
	// Keep the overlay visible while dragging
	if (e.dataTransfer?.types.includes("Files")) {
		isDragOver = true;
	}
}

async function handleDrop(e: DragEvent) {
	e.preventDefault();
	console.log("Drop detected!", e.dataTransfer?.files);

	// Hide the overlay when files are dropped
	isDragOver = false;
	dragCounter = 0;

	// Handle file upload based on environment
	if (e.dataTransfer?.files && e.dataTransfer.files.length > 0) {
		await handleFileUpload(e.dataTransfer.files);
	}
}

async function handleFileUpload(files: FileList) {
	try {
		if (apiClient.environment === "web") {
			// In web mode, upload files directly via HTTP with progress tracking
			const uploadFiles = uploadActions.startUpload(files);

			// Set all files to uploading status
			for (const file of uploadFiles) {
				uploadActions.setFileStatus(file.id, "uploading");
			}

			// For simplicity, treat all files as one upload batch
			// In a more sophisticated implementation, you could upload files individually
			const totalProgress = { current: 0 };

			await apiClient.uploadFileList(
				files,
				(progress) => {
					// Update progress for all files proportionally
					for (const file of uploadFiles) {
						uploadActions.updateFileProgress(file.id, progress, "uploading");
					}
					totalProgress.current = progress;
				},
				(xhr) => {
					// Store the XMLHttpRequest for cancellation
					uploadActions.setCurrentRequest(xhr);
				},
			);

			// Mark all files as processing (they're now being handled by the server)
			for (const file of uploadFiles) {
				uploadActions.setFileStatus(file.id, "processing");
			}

			// Listen for completion via WebSocket events
			// The files will be marked as completed when queue updates come through

			toastStore.success(
				$t("common.common.success"),
				$t("common.messages.files_added_description"),
			);
		}
		// In Wails mode, the backend OnFileDrop handler in main.go processes files automatically
	} catch (error) {
		const err = error as Error;
		console.error("File upload failed:", error);

		// Mark all files as error
		const uploadFiles = uploadActions.startUpload(files);
		for (const file of uploadFiles) {
			uploadActions.setError(file.id, String(error));
		}

		if (err.message !== "Upload cancelled") {
			toastStore.error($t("common.common.error"), String(error));
		}
	}
}

async function handleUpload() {
	try {
		if (apiClient.environment === "wails") {
			// In Wails mode, use the existing upload flow
			await apiClient.uploadFiles();
		} else {
			// In web mode, trigger file input dialog
			const input = document.createElement("input");
			input.type = "file";
			input.multiple = true;
			input.onchange = async (e) => {
				const files = (e.target as HTMLInputElement).files;
				if (files && files.length > 0) {
					await handleFileUpload(files);
				}
			};
			input.click();
		}
	} catch (error) {
		console.error("Upload failed:", error);
		const errorMessage = String(error);

		if (errorMessage.includes("configuration required")) {
			toastStore.error(
				$t("common.common.error"),
				$t("common.messages.error_saving"),
			);
			// Navigate to settings
			if (apiClient.environment === "wails") {
				await apiClient.navigateToSettings();
			} else {
				goto("/settings");
			}
		} else if (errorMessage.includes("runtime not available")) {
			toastStore.error($t("common.common.error"), $t("common.common.loading"));
		} else if (!errorMessage.includes("Upload cancelled")) {
			toastStore.error($t("common.common.error"), errorMessage);
		}
	}
}
</script>

<svelte:head>
  <title>{$t('dashboard.title')} - Postie</title>
  <meta name="description" content="Manage your uploads and monitor progress" />
</svelte:head>

<div style="--wails-drop-target: drop">
  <!-- Drag and Drop Overlay -->
  {#if isDragOver}
    <div class="drag-overlay" style="--wails-drop-target: drop">
      <div class="drag-overlay-content">
        <div class="drag-icon">
          <Plus class="w-16 h-16 text-white" />
        </div>
        <h2 class="text-2xl font-bold text-white mb-2">{$t('dashboard.drag_drop_title')}</h2>
        <p class="text-white/80">{$t('dashboard.drag_drop_description')}</p>
      </div>
    </div>
  {/if}
	<div class="space-y-8 relative">
		<DashboardHeader {needsConfiguration} {criticalConfigError} {handleUpload} />

		<div
			class="flex flex-col gap-8"
			class:pointer-events-none={needsConfiguration || criticalConfigError}
			class:opacity-50={needsConfiguration || criticalConfigError}
		>
			<!-- Queue Stats Overview -->
			<QueueStats />

			<!-- Main Content Area -->
			<div class="space-y-8">
				<ProgressSection />
				<QueueSection />
			</div>

			<!-- Other -->
			<div class="space-y-8">
				<ProviderStatus />
				<WatcherStatus />
			</div>
		</div>
    </div>
</div>
