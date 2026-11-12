<script lang="ts">
import { t } from "$lib/i18n";
import type { backend } from "$lib/wailsjs/go/models";

let { item }: { item: backend.QueueItem } = $props();

let pct = $derived(
	item.totalArticles && item.totalArticles > 0
		? Math.min(1, (item.verifiedArticles ?? 0) / item.totalArticles)
		: 0,
);
</script>

{#if item.status === "complete" && item.verificationStatus === "reused"}
	<span class="badge badge-soft badge-sm">{$t("dashboard.queue.reused_nzb")}</span>
{:else if item.status === "complete" && item.verificationStatus === "pending_verification"}
	<div class="flex flex-col gap-1 shrink-0 min-w-24">
		<span class="inline-flex items-center gap-1.5 text-xs text-warning-ink">
			<span class="loading loading-spinner loading-xs"></span>
			<span class="text-base-content/80">{$t("dashboard.queue.verification.pending")}</span>
		</span>
		{#if item.totalArticles && item.totalArticles > 0}
			<div
				class="progress-track"
				style="height: 0.25rem"
				role="progressbar"
				aria-valuenow={Math.round(pct * 100)}
				aria-valuemin={0}
				aria-valuemax={100}
			>
				<div
					class="progress-fill"
					style="transform: scaleX({pct}); background-color: var(--color-warning)"
				></div>
			</div>
			<span class="text-xs text-base-content/60 tabular-nums">
				{$t("dashboard.queue.verification.progress", {
					values: { verified: item.verifiedArticles ?? 0, total: item.totalArticles },
				})}
			</span>
		{/if}
	</div>
{:else if item.status === "complete" && item.verificationStatus === "verification_failed"}
	<span class="inline-flex items-center gap-1.5 text-xs text-error shrink-0">
		<span class="size-1.5 rounded-full bg-current"></span>
		<span class="text-base-content/80">{$t("dashboard.queue.verification.failed")}</span>
	</span>
{/if}
