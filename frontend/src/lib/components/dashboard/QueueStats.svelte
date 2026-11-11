<script lang="ts">
import apiClient from "$lib/api/client";
import { t } from "$lib/i18n";
import type { backend } from "$lib/wailsjs/go/models";
import { fade } from "$lib/motion";
import StatValue from "./StatValue.svelte";
import { onDestroy, onMount } from "svelte";

let queueStats: backend.QueueStats = {
	total: 0,
	pending: 0,
	running: 0,
	complete: 0,
	error: 0,
	pendingVerification: 0,
	verificationFailed: 0,
};

let intervalId: ReturnType<typeof setInterval> | undefined;
let debounceTimer: ReturnType<typeof setTimeout> | undefined;

function startPolling() {
	if (intervalId) return;
	intervalId = setInterval(loadQueueStats, 10000);
}

function stopPolling() {
	if (intervalId) {
		clearInterval(intervalId);
		intervalId = undefined;
	}
}

function handleVisibilityChange() {
	if (document.hidden) {
		stopPolling();
	} else {
		loadQueueStats();
		startPolling();
	}
}

onMount(async () => {
	// Listen for queue updates with debouncing to prevent double-fetches
	await apiClient.on("queue-updated", () => {
		clearTimeout(debounceTimer);
		debounceTimer = setTimeout(loadQueueStats, 100);
	});

	// Set up polling (pauses when tab inactive)
	startPolling();
	document.addEventListener("visibilitychange", handleVisibilityChange);

	// Load initial stats
	loadQueueStats();
});

onDestroy(async () => {
	// Clean up event listener and timers
	await apiClient.off("queue-updated");
	clearTimeout(debounceTimer);
	stopPolling();
	document.removeEventListener("visibilitychange", handleVisibilityChange);
});

let loadInFlight = false;

async function loadQueueStats() {
	// Skip the tick instead of stacking requests while a previous (slow)
	// request is still pending.
	if (loadInFlight) return;
	loadInFlight = true;
	try {
		queueStats = await apiClient.getQueueStats();
	} catch (error) {
		console.error("Failed to load queue stats:", error);
	} finally {
		loadInFlight = false;
	}
}
</script>

<section>
  <div class="section-label">{$t('dashboard.stats.queue_stats.title')}</div>
  <div class="settings-group">
    <div class="stat-grid grid grid-cols-2 sm:grid-cols-4" role="group" aria-label={$t('dashboard.stats.queue_stats.overview')}>
      <div class="stat-cell">
        <div class="stat-label">{$t('dashboard.stats.queue_stats.total_items')}</div>
        <StatValue value={queueStats.total} />
      </div>

      <div class="stat-cell">
        <div class="stat-label">{$t('dashboard.stats.pending')}</div>
        <StatValue value={queueStats.pending} tone="warning" />
        <div class="stat-hint">
          {#if queueStats.running > 0}
            <span class="size-1.5 rounded-full bg-primary animate-pulse-soft"></span>
            {queueStats.running} {$t('dashboard.stats.queue_stats.currently_processing')}
          {:else if queueStats.pending > 0}
            {$t('dashboard.stats.queue_stats.waiting_to_process')}
          {/if}
        </div>
      </div>

      <div class="stat-cell">
        <div class="stat-label">{$t('dashboard.stats.complete')}</div>
        <StatValue value={queueStats.complete} tone="success" />
        <div class="stat-hint">
          {#if queueStats.complete > 0}{$t('dashboard.stats.queue_stats.successfully_finished')}{/if}
        </div>
      </div>

      <div class="stat-cell">
        <div class="stat-label">{$t('dashboard.stats.errors')}</div>
        <StatValue value={queueStats.error} tone="error" />
        <div class="stat-hint">
          {#if queueStats.error > 0}{$t('dashboard.stats.queue_stats.failed_to_process')}{/if}
        </div>
      </div>
    </div>

    <!-- Durable verification (only shown when there is verification activity) -->
    {#if queueStats.pendingVerification > 0 || queueStats.verificationFailed > 0}
      <div class="stat-grid grid grid-cols-2 border-t border-[var(--ui-hairline)]" in:fade out:fade>
        <div class="stat-cell">
          <div class="stat-label">{$t('dashboard.stats.queue_stats.pending_verification')}</div>
          <StatValue value={queueStats.pendingVerification} />
          <div class="stat-hint">{$t('dashboard.stats.queue_stats.pending_verification_hint')}</div>
        </div>
        <div class="stat-cell">
          <div class="stat-label">{$t('dashboard.stats.queue_stats.verification_failed')}</div>
          <StatValue value={queueStats.verificationFailed} tone="error" />
          <div class="stat-hint">
            {#if queueStats.verificationFailed > 0}{$t('dashboard.stats.queue_stats.verification_failed_hint')}{/if}
          </div>
        </div>
      </div>
    {/if}
  </div>
</section>

<style>
  .stat-cell {
    padding: 0.625rem 1rem;
    min-width: 0;
  }
  /* hairline dividers: the grid background shows through the 1px gaps */
  .stat-grid {
    gap: 1px;
    background: var(--ui-hairline);
  }
  .stat-cell {
    background: var(--color-base-100);
  }
  .stat-label {
    font-size: 0.75rem;
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
  }
  .stat-hint {
    display: flex;
    align-items: center;
    gap: 0.375rem;
    min-height: 1rem;
    font-size: 0.6875rem;
    color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
