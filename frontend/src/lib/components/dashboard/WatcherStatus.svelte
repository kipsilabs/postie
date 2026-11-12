<script lang="ts">
import apiClient from "$lib/api/client";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import { onMount, onDestroy } from "svelte";
import { fade, fly, listFlip } from "$lib/motion";
import { AlertCircle, RefreshCw } from "lucide-svelte";
import { watcher } from "$lib/wailsjs/go/models";

let watcherStatuses = $state<watcher.WatcherStatusInfo[]>([]);
let loading = $state(true);
let error = $state<string>("");
let scanning = $state(false);
let statusCheckInterval: NodeJS.Timeout | null = null;

// Whether any watcher is enabled
let anyEnabled = $derived(watcherStatuses.some(s => s.enabled));
let enabledStatuses = $derived(watcherStatuses.filter(s => s.enabled));

// Format next run time for display
function formatNextRun(nextRunISO: string): string {
  if (!nextRunISO) return "";

  const nextRun = new Date(nextRunISO);
  const now = new Date();
  const diffMs = nextRun.getTime() - now.getTime();

  if (diffMs <= 0) return $t("dashboard.watcher.next_run_soon");

  const diffMinutes = Math.floor(diffMs / (1000 * 60));
  const diffHours = Math.floor(diffMs / (1000 * 60 * 60));
  const diffDays = Math.floor(diffMs / (1000 * 60 * 60 * 24));

  if (diffDays > 0) {
    return $t("dashboard.watcher.next_run_days", { values: { count: diffDays } });
  } else if (diffHours > 0) {
    return $t("dashboard.watcher.next_run_hours", { values: { count: diffHours } });
  } else if (diffMinutes > 0) {
    return $t("dashboard.watcher.next_run_minutes", { values: { count: diffMinutes } });
  } else {
    return $t("dashboard.watcher.next_run_soon");
  }
}

function formatAbsoluteTime(nextRunISO: string): string {
  if (!nextRunISO) return "";
  return new Date(nextRunISO).toLocaleString();
}

function getWatcherDisplayName(status: watcher.WatcherStatusInfo, index: number): string {
  if (status.name) return status.name;
  return $t("dashboard.watcher.watcher_number", { values: { n: index + 1 } });
}

async function checkWatcherStatus() {
  try {
    loading = true;
    error = "";
    watcherStatuses = await apiClient.getWatcherStatus();
  } catch (err) {
    error = String(err);
    console.error("Failed to get watcher status:", err);
  } finally {
    loading = false;
  }
}

async function handleScanNow() {
  try {
    scanning = true;
    await apiClient.triggerScan();
    toastStore.success(
      $t("dashboard.watcher.scan_triggered"),
      $t("dashboard.watcher.scan_triggered_description")
    );
  } catch (err) {
    console.error("Failed to trigger scan:", err);
    toastStore.error("Error", String(err));
  } finally {
    scanning = false;
  }
}

onMount(async () => {
  await checkWatcherStatus();
  statusCheckInterval = setInterval(checkWatcherStatus, 30000);
});

onDestroy(() => {
  if (statusCheckInterval) {
    clearInterval(statusCheckInterval);
    statusCheckInterval = null;
  }
});
</script>

{#if anyEnabled}
  <section in:fade out:fade>
    <div class="flex items-center justify-between mb-2">
      <div class="flex items-center gap-2">
        <div class="section-label !m-0">{$t("dashboard.watcher.title")}</div>
        {#if loading}
          <span class="loading loading-spinner loading-xs"></span>
        {:else if error}
          <AlertCircle class="w-4 h-4 text-error" />
        {/if}
      </div>
      <button
        type="button"
        class="btn btn-xs btn-ghost gap-1"
        onclick={handleScanNow}
        disabled={scanning || watcherStatuses.every(s => !s.initialized)}
        title={$t("dashboard.watcher.scan_now")}
      >
        <RefreshCw class="w-3.5 h-3.5 {scanning ? 'animate-spin' : ''}" />
        {$t("dashboard.watcher.scan_now")}
      </button>
    </div>

    {#if error}
      <div class="text-sm text-error">{$t("dashboard.watcher.error_loading")}</div>
    {:else if !loading || watcherStatuses.length > 0}
      <div class="settings-group">
        {#each enabledStatuses as status, i (status.watch_directory || i)}
          <div class="settings-row !items-start" in:fly out:fade animate:listFlip>
            <div class="flex items-start gap-2.5 min-w-0">
              <span
                class="size-2 rounded-full shrink-0 mt-1.5 {status.initialized ? 'bg-success' : 'bg-warning'}"
                title={status.initialized ? $t("dashboard.watcher.status_active") : $t("dashboard.watcher.status_inactive")}
              ></span>
              <div class="min-w-0">
                <div class="settings-row-title text-sm">{getWatcherDisplayName(status, i)}</div>
                <div class="settings-row-desc truncate" title={status.watch_directory}>
                  {status.watch_directory || $t("dashboard.watcher.no_directory")}
                </div>
                {#if status.schedule}
                  <div class="settings-row-desc tabular-nums">
                    {$t("dashboard.watcher.schedule")}: {status.schedule.start_time} - {status.schedule.end_time}
                    {#if !status.is_within_schedule}
                      <span class="text-warning-ink">({$t("dashboard.watcher.outside_schedule")})</span>
                    {/if}
                  </div>
                {/if}
                {#if !status.initialized}
                  <div class="text-xs text-warning-ink">{$t("dashboard.watcher.not_initialized")}</div>
                {:else if !status.is_within_schedule}
                  <div class="text-xs text-base-content/60">{$t("dashboard.watcher.waiting_for_schedule")}</div>
                {/if}
              </div>
            </div>
            {#if status.next_run && status.initialized}
              <div class="text-xs text-base-content/60 text-right shrink-0 tabular-nums" title={formatAbsoluteTime(status.next_run)}>
                {$t("dashboard.watcher.next_scan")}: {formatNextRun(status.next_run)}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </section>
{/if}
