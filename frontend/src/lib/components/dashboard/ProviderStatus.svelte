<script lang="ts">
import apiClient from "$lib/api/client";
import { startLiveFallback } from "$lib/api/live-fallback";
import {
	EVENT_NNTP_POOL_METRICS_UPDATED,
	type NntpPoolMetricsEvent,
} from "$lib/api/events";
import { t } from "$lib/i18n";
import { classifyProvider, type ProviderSnapshot, type ProviderState } from "$lib/provider-status";
import { backend } from "$lib/wailsjs/go/models";
import { fade } from "$lib/motion";
import { AlertCircle, WifiOff } from "lucide-svelte";
import { onDestroy, onMount } from "svelte";
  import { formatElapsed } from '$lib/utils';

let poolMetrics = $state<backend.NntpPoolMetrics | null>(null);
let previousByHost = new Map<string, ProviderSnapshot>();
let statesByHost = $state<Map<string, ProviderState>>(new Map());

function updateStates(metrics: backend.NntpPoolMetrics | null) {
	const next = new Map<string, ProviderState>();
	const seen = new Map<string, ProviderSnapshot>();
	for (const provider of metrics?.providers ?? []) {
		next.set(provider.host, classifyProvider(provider, previousByHost.get(provider.host)));
		seen.set(provider.host, {
			activeConnections: provider.activeConnections,
			totalErrors: provider.totalErrors,
		});
	}
	previousByHost = seen;
	statesByHost = next;
}

function stateOf(provider: backend.NntpProviderMetrics): ProviderState {
	return statesByHost.get(provider.host) ?? "idle";
}
let initialLoad = $state(true);
let error = $state("");
let stopFallback: (() => void) | undefined;
let destroyed = false;

function applyPoolMetrics(data: unknown) {
	if (!data) return;
	poolMetrics = data as NntpPoolMetricsEvent;
	updateStates(poolMetrics);
	error = "";
}

async function fetchProviderStatus() {
	try {
		error = "";
		poolMetrics = await apiClient.getNntpPoolMetrics();
		updateStates(poolMetrics);
	} catch (err) {
		console.error("Failed to fetch provider status:", err);
		error = String(err);
		poolMetrics = null;
	} finally {
		initialLoad = false;
	}
}

function getProviderDotClass(provider: backend.NntpProviderMetrics) {
	switch (stateOf(provider)) {
		case "connected":
			return "bg-success";
		case "failed":
			return "bg-error";
		default:
			return "bg-base-content/30";
	}
}

function getProviderStatusClass(provider: backend.NntpProviderMetrics) {
	switch (stateOf(provider)) {
		case "connected":
			return "text-success";
		case "failed":
			return "text-error";
		default:
			return "text-base-content/65";
	}
}

function getProviderStatusText(provider: backend.NntpProviderMetrics) {
	return $t(`dashboard.provider.status.${stateOf(provider)}`);
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

onMount(async () => {
	await fetchProviderStatus();
	// Do not let an abandoned fetch register over a newly mounted panel.
	if (destroyed) return;
	await apiClient.on(EVENT_NNTP_POOL_METRICS_UPDATED, applyPoolMetrics);
	stopFallback = startLiveFallback({
		intervalMs: 5000,
		isLive: () => apiClient.isLive(),
		refresh: fetchProviderStatus,
		onReconnect: (cb) => apiClient.onReconnect(cb),
	});
});

onDestroy(() => {
	destroyed = true;
	stopFallback?.();
	apiClient.off(EVENT_NNTP_POOL_METRICS_UPDATED, applyPoolMetrics);
});
</script>

<section>
	<div class="section-label">{$t("dashboard.provider.title")}</div>

	{#if initialLoad && !poolMetrics}
		<div class="settings-group">
			<div class="flex justify-center py-6">
				<span class="loading loading-spinner loading-sm"></span>
			</div>
		</div>
	{:else if error}
		<div class="alert alert-error" in:fade>
			<AlertCircle class="w-4 h-4" />
			<span>{$t("dashboard.provider.error")}: {error}</span>
		</div>
	{:else if poolMetrics?.providers && poolMetrics.providers.length > 0}
		<div class="settings-group" in:fade>
			{#each poolMetrics.providers as provider, i (i)}
				<div class="settings-row !block">
					<div class="flex items-center justify-between gap-3">
						<div class="flex items-center gap-2 min-w-0">
							<span class="size-2 rounded-full shrink-0 {getProviderDotClass(provider)}"></span>
							<span class="settings-row-title truncate">{provider.name || provider.host}</span>
							<span class="text-xs {getProviderStatusClass(provider)}">{getProviderStatusText(provider)}</span>
						</div>
						<div class="text-xs text-base-content/60 tabular-nums text-right">
							{$t("dashboard.provider.connections")}: {provider.activeConnections}/{provider.maxConnections}
							{#if provider.availableSlots > 0}
								({provider.availableSlots} {$t("dashboard.provider.free_slots")})
							{/if}
						</div>
					</div>

					<div class="mt-1.5 flex flex-wrap gap-x-5 gap-y-0.5 text-xs text-base-content/60 tabular-nums">
						<span>{$t("dashboard.provider.ttfb")}: <span class="text-base-content">{provider.ttfb || "—"}</span></span>
						<span>{$t("dashboard.provider.inflight")}: <span class="text-base-content">{provider.inflight}</span></span>
						<span>{$t("dashboard.provider.missing")}: <span class="text-base-content">{provider.missing.toLocaleString()}</span></span>
						<span>{$t("dashboard.provider.errors")}: <span class={provider.totalErrors > 0 ? "text-error" : "text-base-content"}>{provider.totalErrors.toLocaleString()}</span></span>
					</div>

					{#if provider.quotaBytes > 0}
						{@const quotaRatio = Math.min(1, provider.quotaUsed / provider.quotaBytes)}
						<div class="mt-2">
							<div class="flex justify-between items-center gap-2 text-xs mb-1">
								<span class="text-base-content/60 tabular-nums">
									{$t("dashboard.provider.quota")}:
									<span class={provider.quotaExceeded ? "text-error" : "text-base-content"}>
										{formatBytes(provider.quotaUsed)} / {formatBytes(provider.quotaBytes)}
									</span>
									{#if provider.quotaExceeded}
										<span class="badge badge-soft badge-error badge-xs ml-1">{$t("dashboard.provider.quota_exceeded")}</span>
									{/if}
								</span>
								{#if provider.quotaResetAt}
									<span class="text-base-content/65">
										{$t("dashboard.provider.quota_resets")}: {new Date(provider.quotaResetAt).toLocaleString()}
									</span>
								{/if}
							</div>
							<div
								class="progress-track"
								style="height: 0.25rem"
								role="progressbar"
								aria-valuenow={Math.round(quotaRatio * 100)}
								aria-valuemin={0}
								aria-valuemax={100}
								aria-label={$t("dashboard.provider.quota")}
							>
								<div
									class="progress-fill"
									style="transform: scaleX({quotaRatio}){provider.quotaExceeded ? '; background-color: var(--color-error)' : ''}"
								></div>
							</div>
						</div>
					{/if}
				</div>
			{/each}

			<!-- Pool summary -->
			{#snippet summaryRow(label: string, value: string)}
				<div class="settings-row !min-h-9 !py-1.5 text-sm">
					<span class="text-base-content/60">{label}</span>
					<span class="tabular-nums">{value}</span>
				</div>
			{/snippet}
			{@render summaryRow($t("dashboard.provider.upload_speed"), poolMetrics.uploadSpeed > 0 ? formatSpeed(poolMetrics.uploadSpeed) : "—")}
			{@render summaryRow($t("dashboard.provider.avg_upload_speed"), poolMetrics.uploadAvgSpeed > 0 ? formatSpeed(poolMetrics.uploadAvgSpeed) : "—")}
			{@render summaryRow($t("dashboard.provider.uploaded"), poolMetrics.bytesUploaded > 0 ? formatBytes(poolMetrics.bytesUploaded) : "—")}
			{@render summaryRow($t("dashboard.provider.elapsed"), formatElapsed(poolMetrics.elapsed))}
			{@render summaryRow($t("dashboard.provider.total_errors"), poolMetrics.totalErrors.toLocaleString())}
		</div>
	{:else}
		<div class="settings-group" in:fade>
			<div class="flex flex-col items-center text-center py-8 text-base-content/60 gap-2">
				<WifiOff class="w-7 h-7 opacity-40" />
				<p class="text-sm">{$t("dashboard.provider.no_providers")}</p>
			</div>
		</div>
	{/if}
</section>
