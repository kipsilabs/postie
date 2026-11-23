<script lang="ts">
import apiClient from "$lib/api/client";
import type { backend } from "$lib/api/models";
import { t } from "$lib/i18n";
import { fade, fly, listFlip } from "$lib/motion";
import { ShieldCheck } from "lucide-svelte";
import { onDestroy, onMount } from "svelte";

// Verification has no push events: poll while the dashboard is open.
const POLL_MS = 3000;

let items = $state<backend.VerificationActivity[]>([]);
let now = $state(Date.now());
let destroyed = false;
let timer: ReturnType<typeof setInterval> | undefined;
let clock: ReturnType<typeof setInterval> | undefined;
let inFlight = false;

async function refresh() {
	if (inFlight || destroyed) return;
	inFlight = true;
	try {
		const next = await apiClient.getVerificationActivity();
		if (!destroyed) items = next ?? [];
	} catch (error) {
		console.error("Failed to fetch verification progress:", error);
	} finally {
		inFlight = false;
	}
}

onMount(() => {
	refresh();
	timer = setInterval(refresh, POLL_MS);
	clock = setInterval(() => (now = Date.now()), 1000);
});

onDestroy(() => {
	destroyed = true;
	clearInterval(timer);
	clearInterval(clock);
});

function fileName(path: string): string {
	return path.split(/[\\/]/).pop() || path;
}

function ratio(item: backend.VerificationActivity): number {
	return item.totalArticles > 0 ? Math.min(1, item.verifiedArticles / item.totalArticles) : 0;
}

function secondsUntil(at: string | null | undefined): number {
	if (!at) return 0;
	return Math.max(0, Math.ceil((Date.parse(at) - now) / 1000));
}

function phaseDot(phase: backend.VerificationActivity["phase"]): string {
	switch (phase) {
		case "checking":
			return "bg-primary animate-pulse-soft";
		case "repairing":
			return "bg-warning animate-pulse-soft";
		case "deferred":
			return "bg-warning";
		default:
			return "bg-info";
	}
}

function detail(item: backend.VerificationActivity): string {
	switch (item.phase) {
		case "checking":
			return $t("dashboard.verification.detail.checking", {
				values: { verified: item.verifiedArticles, total: item.totalArticles },
			});
		case "repairing":
			return $t("dashboard.verification.detail.repairing", {
				values: { missing: item.missingArticles },
			});
		case "waiting":
			return $t("dashboard.verification.detail.waiting", {
				values: { seconds: secondsUntil(item.nextCheckAt) },
			});
		case "deferred":
			return $t("dashboard.verification.detail.deferred");
		default:
			return $t("dashboard.verification.detail.queued");
	}
}
</script>

{#if items.length > 0}
	<section in:fade>
		<div class="flex items-center justify-between mb-2">
			<div class="section-label !m-0">{$t("dashboard.verification.title")}</div>
			<div class="text-xs text-base-content/60 tabular-nums">
				{$t("dashboard.verification.count", { values: { count: items.length } })}
			</div>
		</div>
		<div class="settings-group">
			{#each items as item (item.id)}
				{@const pct = ratio(item)}
				<div class="settings-row !block" in:fly out:fade animate:listFlip>
					<div class="flex items-center justify-between gap-3">
						<div class="flex items-center gap-2 min-w-0">
							<ShieldCheck class="w-4 h-4 shrink-0 text-base-content/60" />
							<span class="text-sm font-medium truncate" title={item.path}>{fileName(item.path)}</span>
							<span class="inline-flex items-center gap-1.5 text-xs text-base-content/60 shrink-0">
								<span class="size-1.5 rounded-full {phaseDot(item.phase)}"></span>
								{$t(`dashboard.verification.phase.${item.phase}`)}
							</span>
						</div>
						<span class="text-sm tabular-nums shrink-0">{Math.round(pct * 100)}%</span>
					</div>
					<div
						class="progress-track mt-2"
						style="height: 0.25rem"
						role="progressbar"
						aria-valuenow={Math.round(pct * 100)}
						aria-valuemin={0}
						aria-valuemax={100}
						aria-label={fileName(item.path)}
					>
						<div
							class="progress-fill"
							style="transform: scaleX({pct}); {item.phase === 'repairing' ? 'background-color: var(--color-warning)' : ''}"
						></div>
					</div>
					<div class="mt-1.5 text-xs tabular-nums text-base-content/60">{detail(item)}</div>
				</div>
			{/each}
		</div>
	</section>
{/if}
