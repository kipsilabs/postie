<script lang="ts">
import { t } from "$lib/i18n";
import type { backend } from "$lib/api/models";

let { item }: { item: backend.QueueItem } = $props();
</script>

{#if item.status === "complete" && item.verificationStatus === "reused"}
	<span class="badge badge-soft badge-sm">{$t("dashboard.queue.reused_nzb")}</span>
{:else if item.status === "complete" && item.verificationStatus === "pending_verification"}
	<div class="flex flex-col gap-1 shrink-0">
		<span class="inline-flex items-center gap-1.5 text-xs text-warning-ink">
			<span class="loading loading-spinner loading-xs"></span>
			<span class="text-base-content/80">{$t("dashboard.queue.verification.pending")}</span>
		</span>
	</div>
{:else if item.status === "complete" && item.verificationStatus === "verification_failed"}
	<span class="inline-flex items-center gap-1.5 text-xs text-error shrink-0">
		<span class="size-1.5 rounded-full bg-current"></span>
		<span class="text-base-content/80">{$t("dashboard.queue.verification.failed")}</span>
	</span>
{/if}
