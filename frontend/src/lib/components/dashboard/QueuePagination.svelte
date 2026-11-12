<script lang="ts">
import { t } from "$lib/i18n";
import { ChevronLeft, ChevronRight } from "lucide-svelte";

interface Props {
	currentPage: number;
	totalPages: number;
	totalItems: number;
	startItem: number;
	endItem: number;
	hasNext: boolean;
	hasPrev: boolean;
	onPage: (page: number) => void;
}

let { currentPage, totalPages, totalItems, startItem, endItem, hasNext, hasPrev, onPage }: Props = $props();

let pages = $derived(
	Array.from({ length: Math.min(7, totalPages) }, (_, i) => {
		if (totalPages <= 7) return i + 1;
		if (currentPage <= 4) return i + 1;
		if (currentPage >= totalPages - 3) return totalPages - 6 + i;
		return currentPage - 3 + i;
	}),
);
</script>

<div class="flex flex-col sm:flex-row justify-between items-center gap-3 mt-3">
	<div class="text-xs text-base-content/60 tabular-nums">
		{$t("dashboard.queue.showing")} {startItem} {$t("dashboard.queue.to")} {endItem} {$t("dashboard.queue.of")} {totalItems} {$t("dashboard.queue.entries")}
	</div>

	<div class="flex items-center gap-1">
		<button class="btn btn-sm btn-ghost" disabled={!hasPrev} onclick={() => onPage(currentPage - 1)}>
			<ChevronLeft class="w-4 h-4" />
			<span class="hidden sm:inline">{$t("dashboard.queue.previous")}</span>
		</button>

		{#each pages as page (page)}
			<button
				class="btn btn-sm tabular-nums {currentPage === page ? 'btn-primary' : 'btn-ghost'}"
				aria-current={currentPage === page ? "page" : undefined}
				onclick={() => onPage(page)}
			>
				{page}
			</button>
		{/each}

		{#if totalPages > 7 && currentPage < totalPages - 3}
			<span class="text-base-content/50">…</span>
			<button class="btn btn-sm btn-ghost tabular-nums" onclick={() => onPage(totalPages)}>
				{totalPages}
			</button>
		{/if}

		<button class="btn btn-sm btn-ghost" disabled={!hasNext} onclick={() => onPage(currentPage + 1)}>
			<span class="hidden sm:inline">{$t("dashboard.queue.next")}</span>
			<ChevronRight class="w-4 h-4" />
		</button>
	</div>
</div>
