<script lang="ts">
import apiClient from "$lib/api/client";
import { startLiveFallback } from "$lib/api/live-fallback";
import {
  EVENT_PROCESSING_AUTO_PAUSED,
  EVENT_PROCESSING_PAUSED,
  EVENT_PROCESSING_RESUMED,
  EVENT_RUNNING_JOBS_UPDATED,
  type ProcessingPauseEvent,
  type RunningJobsEvent,
} from "$lib/api/events";
import { t } from "$lib/i18n";
import { isUploading, runningJobs } from "$lib/stores/app";
import { toastStore } from "$lib/stores/toast";
import { formatSpeed, formatTime, formatFileSize } from "$lib/utils";
import { ChartPie, CheckCircle, Play, X, Upload, Package, Check } from "lucide-svelte";
import { onMount, onDestroy } from "svelte";

let isPaused = $state(false);
let destroyed = false;
let stopFallback: (() => void) | undefined;

function applyRunningJobs(data: unknown) {
  if (destroyed) return;
  const jobs = (data ?? []) as RunningJobsEvent;
  runningJobs.set(jobs);
}

function applyPauseEvent(data: unknown) {
  const event = data as Partial<ProcessingPauseEvent> | undefined;
  if (event && typeof event.paused === "boolean") {
    isPaused = event.paused;
  }
}

async function fetchInitialState() {
  try {
    const [jobs, paused] = await Promise.all([
      apiClient.getRunningJobDetails(),
      apiClient.isProcessingPaused(),
    ]);
    if (destroyed) return;
    runningJobs.set(jobs);
    isPaused = paused;
  } catch (error) {
    console.error("Failed to fetch initial progress state:", error);
  }
}

onMount(async () => {
  await fetchInitialState();

  await apiClient.on(EVENT_RUNNING_JOBS_UPDATED, applyRunningJobs);
  await apiClient.on(EVENT_PROCESSING_PAUSED, applyPauseEvent);
  await apiClient.on(EVENT_PROCESSING_RESUMED, applyPauseEvent);
  await apiClient.on(EVENT_PROCESSING_AUTO_PAUSED, applyPauseEvent);
  stopFallback = startLiveFallback({
    intervalMs: 5000,
    isLive: () => apiClient.isLive(),
    refresh: fetchInitialState,
    onReconnect: (cb) => apiClient.onReconnect(cb),
  });
});

onDestroy(() => {
  destroyed = true;
  stopFallback?.();
  apiClient.off(EVENT_RUNNING_JOBS_UPDATED, applyRunningJobs);
  apiClient.off(EVENT_PROCESSING_PAUSED, applyPauseEvent);
  apiClient.off(EVENT_PROCESSING_RESUMED, applyPauseEvent);
  apiClient.off(EVENT_PROCESSING_AUTO_PAUSED, applyPauseEvent);
});

// Function to get icon for progress type
function getProgressIcon(type: string) {
  switch (type) {
    case "uploading":
      return Upload;
    case "par2_generation":
      return Package;
    case "checking":
      return Check;
    default:
      return Play;
  }
}

// Function to get color for progress type
function getProgressColor(type: string) {
  switch (type) {
    case "uploading":
      return "text-info bg-info/10";
    case "par2_generation":
      return "text-success bg-success/10";
    case "checking":
      return "text-warning bg-warning/10";
    default:
      return "text-primary bg-primary/10";
  }
}

// Returns the status dot color, text color, and label for a progress task
function getTaskStatus(progressState: any): { dotClass: string; textClass: string; label: string } {
  if (progressState?.IsPaused) {
    return {
      dotClass: "bg-warning animate-pulse",
      textClass: "text-warning",
      label: $t("dashboard.progress.task_status.paused"),
    };
  }
  if (progressState?.IsStarted) {
    return {
      dotClass: "bg-success animate-pulse",
      textClass: "text-success",
      label: $t("dashboard.progress.task_status.in_progress"),
    };
  }
  if (progressState?.IsWaiting) {
    return {
      dotClass: "bg-info animate-pulse",
      textClass: "text-info",
      label: `${$t("dashboard.progress.task_status.waiting")} ${Math.ceil(progressState.WaitSecondsRemaining)}s`,
    };
  }
  return {
    dotClass: "bg-warning",
    textClass: "text-warning",
    label: $t("dashboard.progress.task_status.pending"),
  };
}

async function cancelJob(jobID: string) {
  try {
    await apiClient.cancelJob(jobID);

    // Immediately remove the job from running jobs store as a safety measure
    runningJobs.update((jobs) => jobs.filter((job) => job.id !== jobID));

    toastStore.success(
      $t("common.messages.job_cancelled"),
      $t("common.messages.upload_cancelled_description"),
    );
  } catch (error) {
    console.error("Failed to cancel job:", error);
    toastStore.error($t("common.messages.failed_to_cancel"), String(error));
  }
}

async function cancelDirectUpload() {
  try {
    await apiClient.cancelUpload();
    toastStore.success(
      $t("common.messages.upload_cancelled"),
      $t("common.messages.upload_cancelled_description"),
    );
  } catch (error) {
    console.error("Failed to cancel upload:", error);
    toastStore.error(
      $t("common.messages.failed_to_cancel_upload"),
      String(error),
    );
  }
}

function cancelUpload(jobID: string) {
  if (jobID) {
    cancelJob(jobID);
  } else {
    cancelDirectUpload();
  }
}
</script>

<div class="space-y-6">
  <!-- Header -->
  <div class="flex items-center gap-3 mb-6">
    <div class="p-2 rounded-lg bg-gradient-to-br from-success to-info">
      <ChartPie class="w-6 h-6 text-white" />
    </div>
    <div>
      <h2 class="text-xl font-semibold">
        {$t("dashboard.progress.title")}
      </h2>
      <div class="flex items-center gap-3 mt-1">
        <div class="flex items-center gap-2 px-3 py-1.5 rounded-full bg-base-300/50">
          <div
            class="w-2 h-2 rounded-full transition-all duration-300 {$isUploading
              ? isPaused ? 'bg-warning animate-pulse shadow-lg shadow-warning/50' : 'bg-primary animate-pulse shadow-lg shadow-primary/50'
              : 'bg-base-content/40'}"
          ></div>
          <span class="text-sm font-medium text-base-content/80">
            {$isUploading 
              ? isPaused 
                ? $t("dashboard.progress.status.paused") 
                : $t("dashboard.progress.status.active") 
              : $t("dashboard.progress.status.idle")}
          </span>
        </div>
      </div>
    </div>
  </div>

  {#if $isUploading}
    <!-- Running Jobs with Progress -->
    {#each [...$runningJobs].sort((a, b) => (a.fileName ?? '').localeCompare(b.fileName ?? '')) as job (job.id)}
      <div class="card bg-base-100 shadow-xl p-6 hover:shadow-2xl transition-all duration-200">
        <div class="space-y-6">
          <div class="flex items-center justify-between">
            <div class="flex items-center gap-3">
              <div class="p-2 rounded-full bg-primary/10">
                <Play class="w-5 h-5 text-primary" />
              </div>
              <div>
                <h3 class="text-lg font-semibold text-base-content">
                  {job.fileName}
                </h3>
              </div>
            </div>
            <button
              type="button"
              onclick={() => cancelUpload(job.id)}
              class="btn btn-outline btn-sm flex items-center gap-2"
            >
              <X class="w-4 h-4" />
              {$t("dashboard.progress.cancel_upload")}
            </button>
          </div>

          <!-- Individual Progress Indicators -->
          {#if job.progress.length > 0}
            <div class="space-y-4">
              <h4 class="text-md font-medium text-base-content">{$t('dashboard.progress.active_tasks')}</h4>
              {#each job.progress as progressState}
                {@const IconComponent = getProgressIcon(progressState?.Type)}
                <div class="bg-base-100 rounded-xl border border-base-300 p-4">
                  <div class="flex items-center justify-between mb-3">
                    <div class="flex items-center gap-3">
                      <div class="p-2 rounded-full {getProgressColor(progressState?.Type)}">
                        <IconComponent class="w-4 h-4" />
                      </div>
                      <div>
                        <div class="flex items-center gap-2">
                          <p class="text-sm font-medium text-base-content">
                            {progressState?.Description || progressState?.Type}
                          </p>
                          <!-- Status indicator based on IsStarted, IsPaused, IsWaiting -->
                          <div class="flex items-center gap-1">
                            <div class="w-2 h-2 rounded-full {getTaskStatus(progressState).dotClass}"></div>
                            <span class="text-xs font-medium {getTaskStatus(progressState).textClass}">
                              {getTaskStatus(progressState).label}
                            </span>
                          </div>
                        </div>
                        <p class="text-xs text-base-content/60 capitalize">
                          {progressState?.Type.replace('_', ' ')}
                        </p>
                      </div>
                    </div>
                    <div class="text-right">
                      <span class="text-sm font-semibold {progressState?.IsPaused ? 'text-warning bg-warning/10' : 'text-primary bg-primary/10'} px-2 py-1 rounded-md">
                        {Math.round(progressState?.CurrentPercent * 100 || 0)}%
                      </span>
                    </div>
                  </div>
                  
                  <div
                    class="w-full bg-base-300 rounded-full h-2 mb-3"
                    role="progressbar"
                    aria-valuenow={Math.round(progressState?.CurrentPercent * 100 || 0)}
                    aria-valuemin={0}
                    aria-valuemax={100}
                    aria-label={progressState?.Description || progressState?.Type}
                  >
                    <div
                      class="{progressState?.IsPaused ? 'bg-warning' : 'bg-primary'} h-2 rounded-full transition-all duration-300"
                      style="width: {progressState?.CurrentPercent * 100 || 0}%"
                    ></div>
                  </div>

                  <!-- Progress Stats (dimmed when paused) -->
                  <div class="grid grid-cols-2 gap-4 text-xs transition-opacity duration-300 {progressState?.IsPaused ? 'opacity-40' : 'text-base-content/70'}">
                    <div>
                      <span class="block text-base-content/70">{$t('dashboard.progress.elapsed')}</span>
                      <span class="font-medium text-base-content">{formatTime((progressState?.SecondsSince || 0) * 1000)}</span>
                    </div>
                    <div>
                      <span class="block text-base-content/70">{$t('dashboard.progress.remaining')}</span>
                      <span class="font-medium text-base-content">{formatTime((progressState?.SecondsLeft || 0) * 1000)}</span>
                    </div>

                    <!-- Show speed for upload tasks (only when not paused) -->
                    {#if !progressState?.IsPaused && (progressState.Type === "uploading" || progressState.Type === "checking") && progressState?.KBsPerSecond}
                      <div>
                        <span class="block text-base-content/70">{$t('dashboard.progress.speed')}</span>
                        <span class="font-medium text-base-content">{formatSpeed((progressState.KBsPerSecond || 0) * 1024)}</span>
                      </div>
                    {/if}

                    <!-- Hide current/total for par2 generation -->
                    {#if progressState.Type !== "par2_generation"}
                      <div>
                        <span class="block text-base-content/70">{$t('dashboard.progress.current')}</span>
                        <span class="font-medium text-base-content">
                            {formatFileSize(progressState.CurrentBytes)}
                        </span>
                      </div>
                      <div>
                        <span class="block text-base-content/70">{$t('dashboard.progress.total')}</span>
                        <span class="font-medium text-base-content">
                            {formatFileSize(progressState.Max)}
                        </span>
                      </div>
                    {/if}
                  </div>
                </div>
              {/each}
            </div>
          {/if}

          <!-- Job Information -->
          <div class="bg-base-200/50 rounded-xl p-4 space-y-3">
            <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div class="flex justify-between items-center">
                <span class="text-sm text-base-content/70">{$t('dashboard.progress.file_size')}</span>
                <span class="text-sm font-medium text-base-content">
                  {formatFileSize(job.size)}
                </span>
              </div>
              <div class="flex justify-between items-center">
                <span class="text-sm text-base-content/70">{$t('dashboard.progress.path')}</span>
                <span class="text-sm font-medium text-base-content truncate" title="{job.path}">
                  {job.path.split('/').pop()}
                </span>
              </div>
            </div>
          </div>
        </div>
      </div>
    {/each}

  {:else}
    <!-- Empty State -->
    <div class="text-center py-12">
      <div class="w-16 h-16 mx-auto mb-4 p-4 rounded-full bg-base-300">
        <CheckCircle class="w-8 h-8 text-base-content/50" />
      </div>
      <h3 class="text-lg font-medium text-base-content mb-2">
        {$t("dashboard.progress.no_upload_title")}
      </h3>
      <p class="text-base-content/70">
        {$t("dashboard.progress.no_upload_description")}
      </p>
    </div>
  {/if}
</div>