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
import { CheckCircle, Clock, AlertCircle, Server, WifiOff } from "lucide-svelte";
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

function getProviderStatusIcon(provider: backend.NntpProviderMetrics) {
	switch (stateOf(provider)) {
		case "connected":
			return CheckCircle;
		case "failed":
			return AlertCircle;
		default:
			return Clock;
	}
}

function getProviderStatusClass(provider: backend.NntpProviderMetrics) {
	switch (stateOf(provider)) {
		case "connected":
			return "text-success";
		case "failed":
			return "text-error";
		default:
			return "text-base-content/50";
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
	await apiClient.on(EVENT_NNTP_POOL_METRICS_UPDATED, applyPoolMetrics);
	stopFallback = startLiveFallback({
		intervalMs: 5000,
		isLive: () => apiClient.isLive(),
		refresh: fetchProviderStatus,
		onReconnect: (cb) => apiClient.onReconnect(cb),
	});
});

onDestroy(() => {
	stopFallback?.();
	apiClient.off(EVENT_NNTP_POOL_METRICS_UPDATED, applyPoolMetrics);
});
</script>

<div class="card bg-base-100 shadow-sm">
	<div class="card-body">
		<h2 class="card-title text-base-content flex items-center gap-2">
			<Server class="w-5 h-5" />
			{$t("dashboard.provider.title")}
		</h2>

		{#if initialLoad && !poolMetrics}
			<div class="flex justify-center py-4">
				<span class="loading loading-spinner loading-md"></span>
			</div>
		{:else if error}
			<div class="alert alert-error">
				<AlertCircle class="w-4 h-4" />
				<span>{$t("dashboard.provider.error")}: {error}</span>
			</div>
		{:else if poolMetrics?.providers && poolMetrics.providers.length > 0}
			<div class="space-y-3">
				{#each poolMetrics.providers as provider, i (i)}
					{@const StatusIcon = getProviderStatusIcon(provider)}
					<div class="border border-base-300 rounded-lg p-4">
						<div class="flex items-center justify-between mb-3">
							<div class="flex items-center gap-3">
								<StatusIcon class="w-5 h-5 {getProviderStatusClass(provider)}" />
								<div>
									<h3 class="font-semibold text-base-content">
										{provider.name || provider.host}
									</h3>
								</div>
							</div>
							<div class="text-right">
								<div class="text-sm font-medium {getProviderStatusClass(provider)}">
									{getProviderStatusText(provider)}
								</div>
								<div class="text-xs text-base-content/60">
									{$t("dashboard.provider.connections")}: {provider.activeConnections}/{provider.maxConnections}
									{#if provider.availableSlots > 0}
										({provider.availableSlots} {$t("dashboard.provider.free_slots")})
									{/if}
								</div>
							</div>
						</div>

						<div class="grid grid-cols-2 md:grid-cols-4 gap-4 text-sm">
							<div>
								<span class="text-base-content/70">{$t("dashboard.provider.ttfb")}:</span>
								<span class="font-medium ml-1">{provider.ttfb || "—"}</span>
							</div>
							<div>
								<span class="text-base-content/70">{$t("dashboard.provider.inflight")}:</span>
								<span class="font-medium ml-1">{provider.inflight}</span>
							</div>
							<div>
								<span class="text-base-content/70">{$t("dashboard.provider.missing")}:</span>
								<span class="font-medium ml-1">{provider.missing.toLocaleString()}</span>
							</div>
							<div>
								<span class="text-base-content/70">{$t("dashboard.provider.errors")}:</span>
								<span class="font-medium ml-1 {provider.totalErrors > 0 ? 'text-error' : ''}">{provider.totalErrors.toLocaleString()}</span>
							</div>
						</div>

						{#if provider.quotaBytes > 0}
							<div class="mt-3 pt-3 border-t border-base-300">
								<div class="flex justify-between items-center text-sm mb-1">
									<span class="text-base-content/70">
										{$t("dashboard.provider.quota")}:
										<span class="font-medium {provider.quotaExceeded ? 'text-error' : ''}">
											{formatBytes(provider.quotaUsed)} / {formatBytes(provider.quotaBytes)}
										</span>
										{#if provider.quotaExceeded}
											<span class="badge badge-error badge-sm ml-1">{$t("dashboard.provider.quota_exceeded")}</span>
										{/if}
									</span>
									{#if provider.quotaResetAt}
										<span class="text-xs text-base-content/60">
											{$t("dashboard.provider.quota_resets")}: {new Date(provider.quotaResetAt).toLocaleString()}
										</span>
									{/if}
								</div>
								<progress
									class="progress w-full {provider.quotaExceeded ? 'progress-error' : 'progress-primary'}"
									value={provider.quotaUsed}
									max={provider.quotaBytes}
								></progress>
							</div>
						{/if}
					</div>
				{/each}
			</div>

			<!-- Pool Summary -->
			<div class="mt-4 p-3 bg-base-200 rounded-lg">
				<div class="flex justify-between items-center text-sm">
					<span class="text-base-content/70">{$t("dashboard.provider.upload_speed")}:</span>
					<span class="font-medium">{poolMetrics.uploadSpeed > 0 ? formatSpeed(poolMetrics.uploadSpeed) : "—"}</span>
				</div>
				<div class="flex justify-between items-center text-sm mt-1">
					<span class="text-base-content/70">{$t("dashboard.provider.avg_upload_speed")}:</span>
					<span class="font-medium">{poolMetrics.uploadAvgSpeed > 0 ? formatSpeed(poolMetrics.uploadAvgSpeed) : "—"}</span>
				</div>
				<div class="flex justify-between items-center text-sm mt-1">
					<span class="text-base-content/70">{$t("dashboard.provider.uploaded")}:</span>
					<span class="font-medium">{poolMetrics.bytesUploaded > 0 ? formatBytes(poolMetrics.bytesUploaded) : "—"}</span>
				</div>
				<div class="flex justify-between items-center text-sm mt-1">
					<span class="text-base-content/70">{$t("dashboard.provider.elapsed")}:</span>
					<span class="font-medium">{formatElapsed(poolMetrics.elapsed)}</span>
				</div>
				<div class="flex justify-between items-center text-sm mt-1">
					<span class="text-base-content/70">{$t("dashboard.provider.total_errors")}:</span>
					<span class="font-medium">{poolMetrics.totalErrors.toLocaleString()}</span>
				</div>
			</div>
		{:else}
			<div class="text-center py-8 text-base-content/60">
				<WifiOff class="w-12 h-12 mx-auto mb-2 opacity-50" />
				<p>{$t("dashboard.provider.no_providers")}</p>
			</div>
		{/if}
	</div>
</div>
