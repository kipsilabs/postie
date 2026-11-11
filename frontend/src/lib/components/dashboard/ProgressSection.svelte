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
import { fade, fly, listFlip } from "$lib/motion";
import { CheckCircle, Play, X, Upload, Package, Check } from "lucide-svelte";
import { onMount, onDestroy } from "svelte";

let isPaused = $state(false);
let destroyed = false;
let jobsRevision = 0;
let pauseRevision = 0;
let stopFallback: (() => void) | undefined;

function applyRunningJobs(data: unknown) {
  if (destroyed) return;
  jobsRevision++;
  const jobs = (data ?? []) as RunningJobsEvent;
  runningJobs.set(jobs);
}

function applyPauseEvent(data: unknown) {
  const event = data as Partial<ProcessingPauseEvent> | undefined;
  if (event && typeof event.paused === "boolean") {
    pauseRevision++;
    isPaused = event.paused;
  }
}

async function fetchInitialState() {
  const initialJobsRevision = jobsRevision;
  const initialPauseRevision = pauseRevision;
  try {
    const [jobs, paused] = await Promise.all([
      apiClient.getRunningJobDetails(),
      apiClient.isProcessingPaused(),
    ]);
    if (destroyed) return;
    // Events received during this request are newer than its snapshot.
    if (jobsRevision === initialJobsRevision) runningJobs.set(jobs);
    if (pauseRevision === initialPauseRevision) isPaused = paused;
  } catch (error) {
    console.error("Failed to fetch initial progress state:", error);
  }
}

onMount(async () => {
  await apiClient.initialize();
  if (destroyed) return;
  // Subscribe before fetching: completion may be the last event emitted for
  // a job, so a live socket cannot repair a missed update with a later tick.
  await Promise.all([
    apiClient.on(EVENT_RUNNING_JOBS_UPDATED, applyRunningJobs),
    apiClient.on(EVENT_PROCESSING_PAUSED, applyPauseEvent),
    apiClient.on(EVENT_PROCESSING_RESUMED, applyPauseEvent),
    apiClient.on(EVENT_PROCESSING_AUTO_PAUSED, applyPauseEvent),
  ]);
  if (destroyed) return;
  await fetchInitialState();
  if (destroyed) return;

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

// Returns the status dot color, text color, and label for a progress task
function getTaskStatus(progressState: any): { dotClass: string; label: string; active: boolean } {
  if (progressState?.IsPaused) {
    return { dotClass: "bg-warning", label: $t("dashboard.progress.task_status.paused"), active: false };
  }
  if (progressState?.IsStarted) {
    return { dotClass: "bg-success", label: $t("dashboard.progress.task_status.in_progress"), active: true };
  }
  if (progressState?.IsWaiting) {
    return {
      dotClass: "bg-info",
      label: `${$t("dashboard.progress.task_status.waiting")} ${Math.ceil(progressState.WaitSecondsRemaining)}s`,
      active: true,
    };
  }
  return { dotClass: "bg-base-content/40", label: $t("dashboard.progress.task_status.pending"), active: false };
}

function ratio(progressState: any): number {
  return Math.min(1, Math.max(0, progressState?.CurrentPercent || 0));
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

<section>
  <div class="flex items-center justify-between mb-2">
    <div class="section-label !m-0">{$t("dashboard.progress.title")}</div>
    <div class="flex items-center gap-1.5 text-xs text-base-content/60">
      <span
        class="size-1.5 rounded-full {$isUploading ? (isPaused ? 'bg-warning' : 'bg-primary animate-pulse-soft') : 'bg-base-content/30'}"
      ></span>
      {$isUploading
        ? isPaused
          ? $t("dashboard.progress.status.paused")
          : $t("dashboard.progress.status.active")
        : $t("dashboard.progress.status.idle")}
    </div>
  </div>

  {#if $isUploading}
    <div class="space-y-3">
      {#each [...$runningJobs].sort((a, b) => (a.fileName ?? '').localeCompare(b.fileName ?? '')) as job (job.id)}
        <div class="settings-group" in:fly out:fade animate:listFlip>
          <!-- Job header -->
          <div class="settings-row">
            <div class="min-w-0">
              <div class="settings-row-title truncate" title={job.path}>{job.fileName}</div>
              <div class="settings-row-desc tabular-nums truncate">
                {$t('dashboard.progress.file_size')}: {formatFileSize(job.size)}
                <span aria-hidden="true">·</span>
                {$t('dashboard.progress.path')}: <span title={job.path}>{job.path.split('/').pop()}</span>
              </div>
            </div>
            <button
              type="button"
              onclick={() => cancelUpload(job.id)}
              class="btn btn-ghost btn-sm text-error shrink-0"
              title={$t("dashboard.progress.cancel_upload")}
            >
              <X class="w-4 h-4" />
              <span class="hidden sm:inline">{$t("dashboard.progress.cancel_upload")}</span>
            </button>
          </div>

          <!-- Active tasks -->
          {#each job.progress as progressState}
            {@const IconComponent = getProgressIcon(progressState?.Type)}
            {@const status = getTaskStatus(progressState)}
            {@const pct = ratio(progressState)}
            <div class="settings-row !block" in:fade>
              <div class="flex items-center justify-between gap-3">
                <div class="flex items-center gap-2 min-w-0">
                  <IconComponent class="w-4 h-4 shrink-0 text-base-content/60" />
                  <span class="text-sm font-medium truncate">{progressState?.Description || progressState?.Type}</span>
                  <span class="inline-flex items-center gap-1.5 text-xs text-base-content/60 shrink-0">
                    <span class="size-1.5 rounded-full {status.dotClass} {status.active ? 'animate-pulse-soft' : ''}"></span>
                    {status.label}
                  </span>
                </div>
                <span class="text-sm tabular-nums shrink-0 {progressState?.IsPaused ? 'text-warning-ink' : 'text-base-content'}">
                  {Math.round(pct * 100)}%
                </span>
              </div>

              <div
                class="progress-track mt-2"
                style="height: 0.25rem"
                role="progressbar"
                aria-valuenow={Math.round(pct * 100)}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-label={progressState?.Description || progressState?.Type}
              >
                <div
                  class="progress-fill"
                  style="transform: scaleX({pct}); {progressState?.IsPaused ? 'background-color: var(--color-warning)' : ''}"
                ></div>
              </div>

              <!-- Secondary stats (dimmed when paused) -->
              <div class="mt-1.5 flex flex-wrap gap-x-4 gap-y-0.5 text-xs tabular-nums text-base-content/60 transition-opacity {progressState?.IsPaused ? 'opacity-50' : ''}">
                <span>{$t('dashboard.progress.elapsed')} {formatTime((progressState?.SecondsSince || 0) * 1000)}</span>
                <span>{$t('dashboard.progress.remaining')} {formatTime((progressState?.SecondsLeft || 0) * 1000)}</span>
                {#if !progressState?.IsPaused && (progressState.Type === "uploading" || progressState.Type === "checking") && progressState?.KBsPerSecond}
                  <span>{$t('dashboard.progress.speed')} {formatSpeed((progressState.KBsPerSecond || 0) * 1024)}</span>
                {/if}
                {#if progressState.Type !== "par2_generation"}
                  <span>{formatFileSize(progressState.CurrentBytes)} / {formatFileSize(progressState.Max)}</span>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {/each}
    </div>
  {:else}
    <!-- Empty state -->
    <div class="settings-group" in:fade>
      <div class="flex flex-col items-center text-center py-8 px-4 gap-1">
        <CheckCircle class="w-7 h-7 text-base-content/30 mb-1" />
        <h3 class="text-sm font-medium">{$t("dashboard.progress.no_upload_title")}</h3>
        <p class="text-sm text-base-content/60">{$t("dashboard.progress.no_upload_description")}</p>
      </div>
    </div>
  {/if}
</section>
