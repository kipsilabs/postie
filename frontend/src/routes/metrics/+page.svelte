<script lang="ts">
import apiClient from "$lib/api/client";
import {
	EVENT_NNTP_POOL_METRICS_UPDATED,
	type NntpPoolMetricsEvent,
} from "$lib/api/events";
import { t } from "$lib/i18n";
import { duration, easing } from "$lib/motion";
import { toolbar } from "$lib/stores/toolbar";
import { toastStore } from "$lib/stores/toast";
import { onMount, onDestroy } from "svelte";
import { Tween } from "svelte/motion";
import { Activity, AlertCircle, RotateCw } from "lucide-svelte";
import type { backend } from "$lib/wailsjs/go/models";
import { formatElapsed } from "$lib/utils";

let metrics = $state<backend.NntpPoolMetrics | null>(null);
let loading = $state(true);
let error = $state<string | null>(null);
let transferMetrics = $state<backend.TransferRuntimeMetrics | null>(null);
let transferInterval: ReturnType<typeof setInterval> | undefined;

onMount(async () => {
	await loadMetrics();
	await apiClient.on(EVENT_NNTP_POOL_METRICS_UPDATED, applyMetricsEvent);
	// Transfer-runtime metrics have no push event; poll them every 5s.
	await loadTransferMetrics();
	transferInterval = setInterval(loadTransferMetrics, 5000);
});

onDestroy(() => {
	apiClient.off(EVENT_NNTP_POOL_METRICS_UPDATED, applyMetricsEvent);
	if (transferInterval) clearInterval(transferInterval);
});

async function loadTransferMetrics() {
	try {
		transferMetrics = await apiClient.getTransferRuntimeMetrics();
	} catch (err) {
		console.error("Failed to load transfer runtime metrics:", err);
	}
}

function applyMetricsEvent(data: unknown) {
	if (!data) return;
	metrics = data as NntpPoolMetricsEvent;
	error = null;
}

async function loadMetrics(showLoading = true) {
	try {
		if (showLoading) {
			loading = true;
		}
		error = null;

		metrics = await apiClient.getNntpPoolMetrics();
	} catch (err) {
		console.error("Failed to load NNTP pool metrics:", err);
		error = String(err);
		toastStore.error($t("common.common.error"), $t("metrics.error_loading"));
	} finally {
		if (showLoading) {
			loading = false;
		}
	}
}

function formatBytes(bytes: number): string {
	if (bytes === 0) return "0 B";
	const k = 1024;
	const sizes = ["B", "KB", "MB", "GB", "TB"];
	const i = Math.floor(Math.log(bytes) / Math.log(k));
	return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + " " + sizes[i];
}

function formatSpeed(bytesPerSec: number): string {
	if (bytesPerSec === 0) return "0 B/s";
	return formatBytes(bytesPerSec) + "/s";
}

function formatNumber(num: number): string {
	return num.toLocaleString();
}

// Smoothly animated headline values (first value snaps, later ones tween).
const connectionsTween = new Tween(0);
const avgSpeedTween = new Tween(0);
const bytesTween = new Tween(0);
const errorsTween = new Tween(0);
let tweenReady = false;

$effect(() => {
	const next = metrics;
	if (!next) return;
	const opts = tweenReady
		? { duration: duration("slow"), easing: easing("standard") }
		: { duration: 0 };
	connectionsTween.set(next.activeConnections, opts);
	avgSpeedTween.set(next.avgSpeed, opts);
	bytesTween.set(next.bytesConsumed, opts);
	errorsTween.set(next.totalErrors, opts);
	tweenReady = true;
});

$effect(() => {
	toolbar.set({ actions: metricsActions });
	return () => toolbar.reset();
});
</script>

{#snippet metricsActions()}
	<div class="flex items-center gap-3">
		<div class="hidden items-center gap-1.5 text-xs text-base-content/60 sm:flex">
			<Activity class="h-3.5 w-3.5 text-primary" />
			<span>{$t('metrics.auto_refresh')}</span>
		</div>
		<button
			class="btn btn-sm btn-ghost"
			onclick={() => loadMetrics(true)}
			disabled={loading}
		>
			{#if loading}
				<span class="loading loading-spinner loading-xs"></span>
			{:else}
				<RotateCw class="h-4 w-4" />
			{/if}
			{$t('metrics.refresh')}
		</button>
	</div>
{/snippet}

<div class="space-y-6">
	<p class="text-sm text-base-content/65">{$t('metrics.description')}</p>

	<!-- Transfer runtime (process-wide upload engine + PAR2 scheduler) -->
	{#if transferMetrics && (transferMetrics.uploadWorkerCount > 0 || transferMetrics.par2Capacity > 0)}
		<section>
			<div class="section-label">{$t('metrics.transfer_runtime.title')}</div>
			<div class="grid grid-cols-2 gap-3 md:grid-cols-3">
				<div class="tile animate-fade-up anim-delay" style="--i: 0">
					<div class="tile-title">{$t('metrics.transfer_runtime.upload_workers')}</div>
					<div class="tile-value">{transferMetrics.uploadActiveWorkers}/{transferMetrics.uploadWorkerCount}</div>
					<div class="tile-desc">{transferMetrics.uploadQueuedWorkers} {$t('metrics.transfer_runtime.queued')}</div>
				</div>
				<div class="tile animate-fade-up anim-delay" style="--i: 1">
					<div class="tile-title">{$t('metrics.transfer_runtime.buffer_memory')}</div>
					<div class="tile-value">{formatBytes(transferMetrics.uploadReservedBytes)}</div>
					<div class="tile-desc">/ {formatBytes(transferMetrics.uploadBudgetBytes)}</div>
				</div>
				<div class="tile animate-fade-up anim-delay" style="--i: 2">
					<div class="tile-title">{$t('metrics.transfer_runtime.par2_jobs')}</div>
					<div class="tile-value">{transferMetrics.par2ActiveJobs}/{transferMetrics.par2Capacity}</div>
					<div class="tile-desc">{transferMetrics.par2QueuedJobs} {$t('metrics.transfer_runtime.queued')}</div>
				</div>
			</div>
		</section>
	{/if}

	{#if loading && !metrics}
		<div class="flex items-center justify-center py-12">
			<span class="loading loading-spinner loading-md"></span>
		</div>
	{:else if error}
		<div class="alert alert-error animate-fade-in" role="alert">
			<AlertCircle class="h-4 w-4" />
			<div>
				<h3 class="font-medium">{$t('metrics.error_title')}</h3>
				<div class="text-sm opacity-75">{error}</div>
			</div>
		</div>
	{:else if metrics}
		<!-- Overview -->
		<section class="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-5">
			<div class="tile animate-fade-up anim-delay" style="--i: 0">
				<div class="tile-title">{$t('metrics.active_connections')}</div>
				<div class="tile-value">{Math.round(connectionsTween.current)}</div>
			</div>
			<div class="tile animate-fade-up anim-delay" style="--i: 1">
				<div class="tile-title">{$t('metrics.avg_speed')}</div>
				<div class="tile-value">{formatSpeed(Math.round(avgSpeedTween.current))}</div>
			</div>
			<div class="tile animate-fade-up anim-delay" style="--i: 2">
				<div class="tile-title">{$t('metrics.downloaded')}</div>
				<div class="tile-value">{formatBytes(Math.round(bytesTween.current))}</div>
			</div>
			<div class="tile animate-fade-up anim-delay" style="--i: 3">
				<div class="tile-title">{$t('metrics.elapsed')}</div>
				<div class="tile-value">{formatElapsed(metrics.elapsed) || "—"}</div>
			</div>
			<div class="tile animate-fade-up anim-delay" style="--i: 4">
				<div class="tile-title">{$t('metrics.total_errors')}</div>
				<div class="tile-value" class:text-error={metrics.totalErrors > 0}>{formatNumber(Math.round(errorsTween.current))}</div>
			</div>
		</section>

		<!-- NNTP Server Details -->
		<section class="animate-fade-up anim-delay" style="--i: 5">
			<div class="section-label">{$t('metrics.nntp_servers')} ({metrics.providers.length})</div>
			<div class="overflow-x-auto settings-group">
				<table class="table table-sm w-full tabular-nums">
					<thead>
						<tr>
							<th>{$t('metrics.server_host')}</th>
							<th>{$t('metrics.connections')}</th>
							<th>{$t('metrics.free_slots')}</th>
							<th>{$t('metrics.current_speed')}</th>
							<th>{$t('metrics.avg_speed')}</th>
							<th>{$t('metrics.downloaded')}</th>
							<th>{$t('metrics.missing')}</th>
							<th>{$t('metrics.ttfb')}</th>
							<th>{$t('metrics.ping_rtt')}</th>
							<th>{$t('metrics.errors')}</th>
							<th>{$t('metrics.quota')}</th>
						</tr>
					</thead>
					<tbody>
						{#each metrics.providers as provider}
							<tr>
								<td class="font-medium">{provider.name || provider.host}</td>
								<td>{provider.activeConnections}/{provider.maxConnections}</td>
								<td>{formatNumber(provider.availableSlots)}</td>
								<td>{provider.speedEwma > 0 ? formatSpeed(provider.speedEwma) : "—"}</td>
								<td>{formatSpeed(provider.avgSpeed)}</td>
								<td>{provider.bytesConsumed > 0 ? formatBytes(provider.bytesConsumed) : "—"}</td>
								<td>{formatNumber(provider.missing)}</td>
								<td>{provider.ttfb || "—"}</td>
								<td>{provider.pingRTT || "—"}</td>
								<td class:text-error={provider.totalErrors > 0}>{formatNumber(provider.totalErrors)}</td>
								<td class:text-error={provider.quotaExceeded}>
									{#if provider.quotaBytes > 0}
										{formatBytes(provider.quotaUsed)} / {formatBytes(provider.quotaBytes)}
										{#if provider.quotaExceeded}
											<span class="badge badge-error badge-xs ml-1">{$t('metrics.quota_exceeded')}</span>
										{/if}
									{:else}
										—
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		</section>
	{/if}
</div>

<style>
	.tile {
		min-width: 0;
		padding: 0.75rem 0.875rem;
		border-radius: var(--radius-box);
		background: var(--color-base-100);
		box-shadow: var(--ui-shadow-sm);
	}
	.tile-title {
		font-size: 0.75rem;
		color: color-mix(in oklab, var(--color-base-content) 62%, transparent);
	}
	.tile-value {
		margin-top: 0.125rem;
		font-size: 1.375rem;
		font-weight: var(--ui-font-weight-strong);
		line-height: 1.3;
		font-variant-numeric: tabular-nums;
		overflow-wrap: anywhere;
	}
	.tile-desc {
		font-size: 0.75rem;
		font-variant-numeric: tabular-nums;
		color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
	}
</style>
