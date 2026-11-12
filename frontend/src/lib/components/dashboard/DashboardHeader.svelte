<script lang="ts">
import apiClient from "$lib/api/client";
import {
  EVENT_PROCESSING_AUTO_PAUSED,
  EVENT_PROCESSING_PAUSED,
  EVENT_PROCESSING_RESUMED,
  type ProcessingPauseEvent,
} from "$lib/api/events";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import { uploadActions, uploadStore } from "$lib/stores/upload";
import { isUploading } from "$lib/stores/app";
import { fade, fly } from "$lib/motion";
import { toolbar } from "$lib/stores/toolbar";
import { AlertTriangle, Plus, X, Pause, Play, FolderOpen, FolderPlus } from "lucide-svelte";
import { onMount, onDestroy } from "svelte";
import FileExplorerModal from "./FileExplorerModal.svelte";

let { needsConfiguration, criticalConfigError, handleUpload }: {
  needsConfiguration: boolean;
  criticalConfigError: boolean;
  handleUpload: () => Promise<void>;
} = $props();

let isPaused = $state(false);
let isAutoPaused = $state(false);
let autoPauseReason = $state("");
let showFileExplorer = $state(false);
let isWebMode = $state(false);

function applyPauseEvent(data: unknown) {
  const event = data as Partial<ProcessingPauseEvent> | undefined;
  if (!event) return;
  if (typeof event.paused === "boolean") isPaused = event.paused;
  if (typeof event.autoPaused === "boolean") isAutoPaused = event.autoPaused;
  if (typeof event.reason === "string") autoPauseReason = event.reason;
}

async function fetchInitialPauseStatus() {
  try {
    const [paused, autoPaused, reason] = await Promise.all([
      apiClient.isProcessingPaused(),
      apiClient.isProcessingAutoPaused(),
      apiClient.getAutoPauseReason(),
    ]);
    isPaused = paused;
    isAutoPaused = autoPaused;
    autoPauseReason = reason;
  } catch (error) {
    console.error("Failed to fetch initial pause status:", error);
  }
}

onMount(async () => {
  await apiClient.initialize();
  isWebMode = apiClient.environment === "web";

  await fetchInitialPauseStatus();

  await apiClient.on(EVENT_PROCESSING_PAUSED, applyPauseEvent);
  await apiClient.on(EVENT_PROCESSING_RESUMED, applyPauseEvent);
  await apiClient.on(EVENT_PROCESSING_AUTO_PAUSED, applyPauseEvent);
});

onDestroy(() => {
  apiClient.off(EVENT_PROCESSING_PAUSED, applyPauseEvent);
  apiClient.off(EVENT_PROCESSING_RESUMED, applyPauseEvent);
  apiClient.off(EVENT_PROCESSING_AUTO_PAUSED, applyPauseEvent);
});

function formatFileSize(bytes: number): string {
	if (bytes === 0) return "0 Bytes";
	const k = 1024;
	const sizes = ["Bytes", "KB", "MB", "GB"];
	const i = Math.floor(Math.log(bytes) / Math.log(k));
	return `${Number.parseFloat((bytes / k ** i).toFixed(2))} ${sizes[i]}`;
}

async function cancelCurrentUpload() {
	try {
		// Cancel the upload (this will abort the XMLHttpRequest)
		uploadActions.cancelUpload();

		// Also try to cancel on server side if possible
		try {
			await apiClient.cancelUpload();
		} catch (serverError) {
			// Server cancel may fail, but that's okay since we already cancelled client-side
			console.warn("Server-side cancel failed:", serverError);
		}

		toastStore.success(
			$t("common.messages.upload_cancelled"),
			"Upload has been cancelled",
		);
	} catch (error) {
		console.error("Failed to cancel upload:", error);
		toastStore.error($t("common.common.error"), String(error));
	}
}

async function pauseProcessing() {
  try {
    await apiClient.pauseProcessing();
    isPaused = true;
    toastStore.success(
      $t("dashboard.progress.paused"),
      $t("dashboard.progress.paused_description")
    );
  } catch (error) {
    console.error("Failed to pause processing:", error);
    toastStore.error($t("common.messages.error"), String(error));
  }
}

async function resumeProcessing() {
  try {
    await apiClient.resumeProcessing();
    isPaused = false;
    toastStore.success(
      $t("dashboard.progress.resumed"),
      $t("dashboard.progress.resumed_description")
    );
  } catch (error) {
    console.error("Failed to resume processing:", error);
    toastStore.error($t("common.messages.error"), String(error));
  }  
}

function openFileExplorer() {
  showFileExplorer = true;
}

function closeFileExplorer() {
  showFileExplorer = false;
}

// Handle folder upload - works in both desktop (Wails) and web modes
async function handleFolderUpload() {
  try {
    if (apiClient.environment === "wails") {
      // Desktop mode: use native multi-folder picker
      const folderPaths = await apiClient.selectFolders();
      if (folderPaths.length === 0) return;

      await apiClient.uploadFolders(folderPaths);

      if (folderPaths.length === 1) {
        toastStore.success(
          $t("dashboard.header.folder_added"),
          $t("dashboard.header.folder_added_description")
        );
      } else {
        toastStore.success(
          $t("dashboard.header.folders_added"),
          $t("dashboard.header.folders_added_description", { values: { count: folderPaths.length } })
        );
      }
    } else {
      // Web mode: use hidden input with webkitdirectory attribute
      // Web mode already supports selecting one folder at a time via the browser's native picker;
      // multiple folder uploads require repeated clicks (browser limitation).
      const input = document.createElement("input");
      input.type = "file";
      // @ts-ignore - webkitdirectory is not in the type definitions
      input.webkitdirectory = true;
      // @ts-ignore
      input.directory = true;
      input.multiple = true;

      input.onchange = async () => {
        if (input.files && input.files.length > 0) {
          uploadActions.startUpload(input.files);

          try {
            await apiClient.uploadFileList(
              input.files,
              (progress: number) => {
                uploadActions.updateTotalProgress(progress);
              },
              (xhr: XMLHttpRequest) => {
                uploadActions.setCurrentRequest(xhr);
              }
            );

            uploadActions.completeUpload();
            toastStore.success(
              $t("dashboard.header.folder_added"),
              $t("dashboard.header.folder_added_description")
            );
          } catch (error) {
            console.error("Folder upload failed:", error);
            toastStore.error($t("common.common.error"), String(error));
            uploadActions.cancelUpload();
          }
        }
      };

      input.click();
    }
  } catch (error) {
    console.error("Failed to upload folder:", error);
    toastStore.error($t("common.common.error"), String(error));
  }
}

$effect(() => {
  toolbar.set({ actions: toolbarActions });
  return () => toolbar.reset();
});
</script>

{#snippet toolbarActions()}
  <div class="flex items-center gap-1.5">
    {#if !$uploadStore.isUploading}
      <button class="btn btn-primary btn-sm" onclick={handleUpload} disabled={needsConfiguration}>
        <Plus class="w-4 h-4" />
        {$t("dashboard.header.add_files")}
      </button>
      <button
        class="btn btn-ghost btn-sm"
        onclick={handleFolderUpload}
        disabled={needsConfiguration}
        title={$t("dashboard.header.add_folder_tooltip")}
        aria-label={$t("dashboard.header.add_folder")}
      >
        <FolderPlus class="w-4 h-4" />
        <span class="hidden min-[1100px]:inline">{$t("dashboard.header.add_folder")}</span>
      </button>
      {#if isWebMode}
        <button
          class="btn btn-ghost btn-sm"
          onclick={openFileExplorer}
          disabled={needsConfiguration}
          title={$t("dashboard.header.import_files_tooltip")}
          aria-label={$t("dashboard.header.import_files")}
        >
          <FolderOpen class="w-4 h-4" />
          <span class="hidden min-[1100px]:inline">{$t("dashboard.header.import_files")}</span>
        </button>
      {/if}
    {/if}
    <button
      type="button"
      onclick={isPaused ? resumeProcessing : pauseProcessing}
      class="btn btn-sm {isPaused ? 'btn-warning' : 'btn-ghost'}"
      title={isPaused ? $t("dashboard.progress.resume_processing") : $t("dashboard.progress.pause_processing")}
      aria-label={isPaused ? $t("dashboard.progress.resume_processing") : $t("dashboard.progress.pause_processing")}
    >
      {#if isPaused}
        <Play class="w-4 h-4" />
      {:else}
        <Pause class="w-4 h-4" />
      {/if}
      <span class="hidden min-[1100px]:inline">
        {isPaused ? $t("dashboard.progress.resume_processing") : $t("dashboard.progress.pause_processing")}
      </span>
    </button>
  </div>
{/snippet}

<div class="space-y-3 empty:hidden">
  {#if $uploadStore.isUploading}
    <!-- Browser upload progress -->
    <div class="settings-group" in:fly out:fade>
      <div class="settings-row !block">
        <div class="flex items-center justify-between gap-3">
          <span class="text-sm font-medium">
            Uploading {$uploadStore.files.length} file{$uploadStore.files.length !== 1 ? 's' : ''}
          </span>
          <div class="flex items-center gap-2">
            <span class="text-sm text-base-content/70 tabular-nums">{Math.round($uploadStore.totalProgress)}%</span>
            <button
              class="btn btn-ghost btn-square btn-xs text-error"
              onclick={cancelCurrentUpload}
              title={$t("dashboard.progress.cancel_upload")}
              aria-label={$t("dashboard.progress.cancel_upload")}
            >
              <X class="w-4 h-4" />
            </button>
          </div>
        </div>
        <div
          class="progress-track mt-2"
          style="height: 0.25rem"
          role="progressbar"
          aria-valuenow={Math.round($uploadStore.totalProgress)}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-label="Upload progress"
        >
          <div class="progress-fill" style="transform: scaleX({Math.min(1, Math.max(0, $uploadStore.totalProgress / 100))})"></div>
        </div>
        {#if $uploadStore.files.length > 0}
          {@const currentFile = $uploadStore.files.find(f => f.status === 'uploading' || f.status === 'processing') || $uploadStore.files[0]}
          <div class="mt-1.5 text-xs text-base-content/60 truncate tabular-nums">
            {currentFile.name} ({formatFileSize(currentFile.size)})
          </div>
        {/if}
      </div>
    </div>
  {/if}

  {#if criticalConfigError}
    <div class="alert alert-error alert-soft" in:fly out:fade>
      <AlertTriangle class="w-4 h-4" />
      <span>
        <span class="font-semibold">{$t("dashboard.alerts.config_error")}</span>
        {@html $t("dashboard.alerts.config_error_description", { values: { settingsLink: `<a href="/settings" class="font-medium underline hover:no-underline">${$t("dashboard.alerts.settings_link")}</a>` } })}
      </span>
    </div>
  {:else if needsConfiguration}
    <div class="alert alert-warning alert-soft" in:fly out:fade>
      <AlertTriangle class="w-4 h-4" />
      <span>
        <span class="font-semibold">{$t("dashboard.alerts.config_required")}</span>
        {@html $t("dashboard.alerts.config_required_description", { values: { settingsLink: `<a href="/settings" class="font-medium underline hover:no-underline">${$t("dashboard.alerts.settings_link")}</a>` } })}
      </span>
    </div>
  {/if}

  {#if isAutoPaused && autoPauseReason}
    <div class="alert alert-warning alert-soft" in:fly out:fade>
      <Pause class="w-4 h-4" />
      <span>
        <span class="font-semibold">{$t("dashboard.alerts.auto_paused")}</span>
        {autoPauseReason}
      </span>
    </div>
  {/if}
</div>

<!-- File Explorer Modal -->
<FileExplorerModal isOpen={showFileExplorer} onClose={closeFileExplorer} />
