<script lang="ts">
import { t } from "$lib/i18n";
import type { backend } from "$lib/wailsjs/go/models";
import { Download, RotateCcw, Trash2 } from "lucide-svelte";

interface Props {
	item: backend.QueueItem;
	onDownload: (id: string) => void;
	onRetry: (id: string) => void;
	onRemove: (id: string) => void;
}

let { item, onDownload, onRetry, onRemove }: Props = $props();
</script>

<div class="flex items-center justify-end gap-0.5">
	{#if item.status === "complete"}
		<button
			class="btn btn-ghost btn-square btn-xs"
			onclick={() => onDownload(item.id)}
			title={$t("dashboard.queue.download_nzb")}
			aria-label={$t("dashboard.queue.download_nzb")}
		>
			<Download class="w-4 h-4" />
		</button>
	{/if}
	{#if item.status === "error"}
		<button
			class="btn btn-ghost btn-square btn-xs relative"
			onclick={() => onRetry(item.id)}
			title={$t("dashboard.queue.retry")}
			aria-label={$t("dashboard.queue.retry")}
		>
			<RotateCcw class="w-4 h-4" />
			{#if item.retryCount > 0}
				<sup class="absolute -top-1 -right-1 text-[9px] leading-none bg-warning text-warning-content rounded-full min-w-3.5 h-3.5 flex items-center justify-center font-semibold">{item.retryCount}</sup>
			{/if}
		</button>
	{/if}
	<button
		class="btn btn-ghost btn-square btn-xs text-error"
		onclick={() => onRemove(item.id)}
		title={$t("dashboard.queue.remove_from_queue")}
		aria-label={$t("dashboard.queue.remove_from_queue")}
	>
		<Trash2 class="w-4 h-4" />
	</button>
</div>
