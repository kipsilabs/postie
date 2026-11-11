<script lang="ts">
import { segmentedKeys } from "$lib/actions/segmentedKeys";
import apiClient from "$lib/api/client";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import { fade, fly, listFlip } from "$lib/motion";
import { formatDate, formatDuration, formatFileSize } from "$lib/utils";
import type { backend } from "$lib/wailsjs/go/models";
import {
	ArrowDown,
	ArrowUp,
	ArrowUpDown,
	Clock,
	Inbox,
	RotateCcw,
	Search,
	Trash2,
	X,
} from "lucide-svelte";
import ConfirmDialog from "./ConfirmDialog.svelte";
import QueueItemActions from "./QueueItemActions.svelte";
import QueuePagination from "./QueuePagination.svelte";
import StatusLabel from "./StatusLabel.svelte";
import VerificationBadge from "./VerificationBadge.svelte";
import { onDestroy, onMount } from "svelte";

let paginatedResult = $state<backend.PaginatedQueueResult | null>(null);
let initialLoad = $state(true);
let debounceTimer: ReturnType<typeof setTimeout> | undefined;

// Pagination state - controlled by server
let currentPage = $state(1);
let itemsPerPage = $state(10);
let sortBy = $state("created");
let sortOrder = $state("desc");
let statusFilter = $state(""); // "" = all, "pending", "complete", "error"
let searchQuery = $state("");

// Derived from server response
let queueItems = $derived(paginatedResult?.items ?? []);
let totalPages = $derived(paginatedResult?.totalPages ?? 0);
let totalItems = $derived(paginatedResult?.totalItems ?? 0);
let startItem = $derived(paginatedResult ? Math.max(1, (currentPage - 1) * itemsPerPage + 1) : 0);
let endItem = $derived(paginatedResult ? Math.min(currentPage * itemsPerPage, totalItems) : 0);
let hasNext = $derived(paginatedResult?.hasNext ?? false);
let hasPrev = $derived(paginatedResult?.hasPrev ?? false);

// Client-side search within current page
let filteredItems = $derived(
	searchQuery.trim()
		? queueItems.filter((item) =>
				item.fileName?.toLowerCase().includes(searchQuery.toLowerCase()),
			)
		: queueItems,
);

let intervalId: ReturnType<typeof setInterval> | undefined;
let pendingRemoveId = $state<string | null>(null);
let selectedIds = $state(new Set<string>());
let pendingBatchDelete = $state(false);
const allSelected = $derived(
	filteredItems.length > 0 && filteredItems.every((item) => selectedIds.has(item.id)),
);

function startPolling() {
	if (intervalId) return;
	intervalId = setInterval(loadQueue, 5000);
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
		loadQueue();
		startPolling();
	}
}

onMount(() => {
	apiClient.on("queue-updated", () => {
		clearTimeout(debounceTimer);
		debounceTimer = setTimeout(loadQueue, 100);
	});

	startPolling();
	document.addEventListener("visibilitychange", handleVisibilityChange);

	// Initial load
	loadQueue();
});

onDestroy(() => {
	apiClient.off("queue-updated");
	clearTimeout(debounceTimer);
	stopPolling();
	document.removeEventListener("visibilitychange", handleVisibilityChange);
});

let loadInFlight = false;

async function loadQueue() {
	// Skip the tick instead of stacking requests while a previous (slow)
	// request is still pending.
	if (loadInFlight) return;
	loadInFlight = true;
	try {
		const result = await apiClient.getQueueItems({
			page: currentPage,
			limit: itemsPerPage,
			sortBy: sortBy,
			order: sortOrder,
			status: statusFilter,
		});
		paginatedResult = result;
	} catch (error) {
		console.error("Failed to load queue:", error);
		toastStore.error($t("common.messages.failed_to_load_queue"), String(error));
		paginatedResult = null;
	} finally {
		initialLoad = false;
		loadInFlight = false;
	}
}

async function removeFromQueue(id: string) {
	pendingRemoveId = id;
}

async function confirmRemove() {
	if (!pendingRemoveId) return;
	const id = pendingRemoveId;
	pendingRemoveId = null;

	try {
		await apiClient.removeFromQueue(id);
		await loadQueue();
		toastStore.success(
			$t("common.messages.item_removed"),
			$t("common.messages.item_removed_description"),
		);
	} catch (error) {
		console.error("Failed to remove item from queue:", error);
		toastStore.error($t("common.messages.failed_to_remove_item"), String(error));
	}
}

function cancelRemove() {
	pendingRemoveId = null;
}

function toggleSelectAll() {
	if (allSelected) {
		selectedIds = new Set();
	} else {
		selectedIds = new Set(filteredItems.map((i) => i.id));
	}
}

function toggleItem(id: string) {
	const next = new Set(selectedIds);
	if (next.has(id)) next.delete(id);
	else next.add(id);
	selectedIds = next;
}

async function batchDeleteSelected() {
	pendingBatchDelete = true;
}

async function confirmBatchDelete() {
	const ids = [...selectedIds];
	let successCount = 0;
	for (const id of ids) {
		try {
			await apiClient.removeFromQueue(id);
			successCount++;
		} catch (e) {
			console.error("Failed to remove", id, e);
		}
	}
	selectedIds = new Set();
	pendingBatchDelete = false;
	toastStore.success($t("dashboard.queue.batch_deleted", { values: { count: successCount } }));
	await loadQueue();
}

async function downloadNZB(id: string) {
	try {
		await apiClient.downloadNZB(id);
	} catch (error) {
		console.error("Failed to download NZB:", error);
		toastStore.error($t("common.messages.failed_to_download_nzb"), String(error));
	}
}

async function retryJob(id: string) {
	try {
		await apiClient.retryJob(id);
		await loadQueue();
		toastStore.success($t("common.messages.item_retried"));
	} catch (error) {
		console.error("Failed to retry job:", error);
		toastStore.error($t("common.messages.failed_to_retry_item"), String(error));
	}
}

async function changePriority(id: string, newPriority: number) {
	try {
		await apiClient.setQueueItemPriority(id, newPriority);
		await loadQueue();
	} catch (error) {
		console.error("Failed to update priority:", error);
		toastStore.error($t("common.messages.failed_to_update_priority"), String(error));
	}
}

function goToPage(page: number) {
	if (page >= 1 && page <= totalPages) {
		currentPage = page;
		selectedIds = new Set();
		loadQueue();
	}
}

function changeItemsPerPage(event: Event) {
	const target = event.target as HTMLSelectElement;
	itemsPerPage = Number.parseInt(target.value);
	currentPage = 1;
	selectedIds = new Set();
	loadQueue();
}

function toggleSort(column: string) {
	if (sortBy === column) {
		sortOrder = sortOrder === "asc" ? "desc" : "asc";
	} else {
		sortBy = column;
		sortOrder = "desc";
	}
	currentPage = 1;
	selectedIds = new Set();
	loadQueue();
}

function setStatusFilter(value: string) {
	statusFilter = value;
	currentPage = 1;
	selectedIds = new Set();
	loadQueue();
}

async function clearCompleted() {
	try {
		await apiClient.clearQueue();
		await loadQueue();
		toastStore.success(
			$t("common.messages.queue_cleared"),
			$t("common.messages.queue_cleared_description"),
		);
	} catch (error) {
		console.error("Failed to clear queue:", error);
		toastStore.error($t("common.messages.failed_to_clear_queue"), String(error));
	}
}

async function retryAllFailed() {
	// Fetch all error items across all pages (not just current page)
	let allFailedItems: backend.QueueItem[] = [];
	try {
		const result = await apiClient.getQueueItems({
			page: 1,
			limit: 9999,
			sortBy: "created",
			order: "desc",
			status: "error",
		});
		allFailedItems = result?.items ?? [];
	} catch {
		// Fall back to current page items
		allFailedItems = queueItems.filter((item) => item.status === "error");
	}

	if (allFailedItems.length === 0) return;

	let succeeded = 0;
	for (const item of allFailedItems) {
		try {
			await apiClient.retryJob(item.id);
			succeeded++;
		} catch (error) {
			console.error("Failed to retry item:", item.id, error);
		}
	}

	await loadQueue();
	if (succeeded > 0) {
		toastStore.success($t("dashboard.queue.bulk_retry_success", { values: { count: succeeded } }));
	}
}

function getSortIcon(column: string) {
	if (sortBy !== column) {
		return ArrowUpDown;
	}
	return sortOrder === "asc" ? ArrowUp : ArrowDown;
}

let FilenameIcon = $derived(getSortIcon("filename"));
let SizeIcon = $derived(getSortIcon("size"));
let StatusSortIcon = $derived(getSortIcon("status"));
let PriorityIcon = $derived(getSortIcon("priority"));
let CreatedIcon = $derived(getSortIcon("created"));

const statusFilters = [
	{ value: "", label: "dashboard.queue.filter_all" },
	{ value: "pending", label: "dashboard.queue.filter_pending" },
	{ value: "complete", label: "dashboard.queue.filter_complete" },
	{ value: "error", label: "dashboard.queue.filter_error" },
] as const;
let filterIndex = $derived(Math.max(0, statusFilters.findIndex((f) => f.value === statusFilter)));

function durationOf(item: backend.QueueItem): string {
	const end = item.completedAt ?? (item.status === "error" ? item.updatedAt : null);
	if (!end) return "";
	return formatDuration(new Date(end).getTime() - new Date(item.createdAt).getTime());
}

function truncate(message: string, max: number): string {
	return message.length > max ? `${message.substring(0, max)}…` : message;
}
</script>

{#snippet sortHeader(column: string, label: string, Icon: typeof ArrowUpDown)}
  <button
    type="button"
    class="inline-flex items-center gap-1 select-none hover:text-base-content transition-colors"
    onclick={() => toggleSort(column)}
  >
    {label}
    <Icon class="w-3.5 h-3.5 {sortBy === column ? 'text-primary' : 'opacity-40'}" />
  </button>
{/snippet}

<section class="relative">
  <!-- Header -->
  <div class="flex items-baseline gap-2 mb-2">
    <div class="section-label !m-0">{$t("dashboard.queue.title")}</div>
    <span class="text-xs text-base-content/65 tabular-nums">{totalItems} {$t("dashboard.queue.items")}</span>
  </div>

  {#if !initialLoad}
    <!-- Filter + Search + Controls row -->
    <div class="flex flex-wrap items-center gap-2 mb-3" in:fade>
      <div class="segmented" role="radiogroup" use:segmentedKeys style="--n: {statusFilters.length}; --i: {filterIndex}">
        <span class="segmented-thumb" aria-hidden="true"></span>
        {#each statusFilters as filter (filter.value)}
          <button
            type="button"
            role="radio"
            class="segmented-item !px-3 !text-[0.8125rem]"
            aria-checked={statusFilter === filter.value}
            tabindex={statusFilter === filter.value ? 0 : -1}
            onclick={() => setStatusFilter(filter.value)}
          >
            {$t(filter.label)}
          </button>
        {/each}
      </div>

      <div class="relative">
        <Search class="absolute left-2.5 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-base-content/50 pointer-events-none" />
        <input
          type="search"
          class="input input-sm pl-8 w-44"
          placeholder={$t("dashboard.queue.search_placeholder")}
          bind:value={searchQuery}
        />
      </div>
      {#if searchQuery.trim()}
        <span class="text-xs text-base-content/65 tabular-nums" in:fade>
          {filteredItems.length}/{queueItems.length} — {$t("dashboard.queue.search_within_page")}
        </span>
      {/if}

      <div class="flex items-center gap-2 ml-auto">
        {#if totalItems > 0}
          <button class="btn btn-xs btn-ghost gap-1" onclick={retryAllFailed} title={$t("dashboard.queue.retry_all_failed")}>
            <RotateCcw class="w-3.5 h-3.5" />
            {$t("dashboard.queue.retry_all_failed")}
          </button>
          <button class="btn btn-xs btn-ghost gap-1" onclick={clearCompleted} title={$t("dashboard.header.clear_completed")}>
            <Trash2 class="w-3.5 h-3.5" />
            {$t("dashboard.header.clear_completed")}
          </button>
        {/if}
        <label for="items-per-page" class="text-xs text-base-content/60 hidden sm:block">
          {$t("dashboard.queue.items_per_page")}
        </label>
        <select id="items-per-page" class="select select-sm w-auto" value={itemsPerPage} onchange={changeItemsPerPage}>
          <option value={5}>5</option>
          <option value={10}>10</option>
          <option value={25}>25</option>
          <option value={50}>50</option>
        </select>
      </div>
    </div>
  {/if}

  {#if initialLoad}
    <!-- Loading skeleton -->
    <div class="settings-group">
      <div class="p-4 space-y-3">
        {#each Array(5) as _, i (i)}
          <div class="flex items-center gap-4">
            <div class="skeleton h-4 w-48"></div>
            <div class="skeleton h-4 w-16"></div>
            <div class="skeleton h-4 w-20"></div>
            <div class="flex-1"></div>
            <div class="skeleton h-5 w-5 rounded"></div>
          </div>
        {/each}
      </div>
    </div>
  {:else if totalItems === 0}
    <!-- Empty state -->
    <div class="settings-group" in:fade>
      <div class="flex flex-col items-center text-center py-12 px-4 gap-1">
        <Inbox class="w-8 h-8 text-base-content/30 mb-2" />
        <h3 class="text-sm font-medium">{$t("dashboard.queue.no_items")}</h3>
        <p class="text-sm text-base-content/60">{$t("dashboard.queue.no_items_description")}</p>
        <p class="text-xs text-base-content/65">{$t("dashboard.queue.empty_hint")}</p>
      </div>
    </div>
  {:else}
    <!-- Mobile list -->
    <div class="md:hidden settings-group">
      {#each filteredItems as item (item.id)}
        <div
          class="px-3 py-2.5 border-t border-[var(--ui-hairline)] first:border-t-0"
          class:row-selected={selectedIds.has(item.id)}
          in:fly
          out:fade={{ speed: "fast" }}
          animate:listFlip
        >
          <div class="flex items-start justify-between gap-2">
            <div class="flex items-start gap-2 min-w-0 flex-1">
              <input
                type="checkbox"
                class="checkbox checkbox-sm mt-0.5 shrink-0"
                checked={selectedIds.has(item.id)}
                onclick={() => toggleItem(item.id)}
              />
              <div class="min-w-0 flex-1">
                <div class="text-sm font-medium truncate" title={item.fileName}>{item.fileName}</div>
                <div class="text-xs text-base-content/60 mt-0.5 tabular-nums">{formatFileSize(item.size)}</div>
              </div>
            </div>
            <div class="flex flex-col items-end gap-1 shrink-0">
              {#if !(item.status === "complete" && item.verificationStatus === "pending_verification")}
                <StatusLabel status={item.status} />
              {/if}
              <VerificationBadge {item} />
            </div>
          </div>
          {#if item.errorMessage}
            <p class="text-xs text-error mt-1.5 break-words leading-snug">{truncate(item.errorMessage, 100)}</p>
          {/if}
          <div class="flex items-center justify-between mt-1.5">
            <div class="text-xs text-base-content/65 tabular-nums">
              {formatDate(item.createdAt)}
              {#if durationOf(item)}
                <span class="ml-2 inline-flex items-center gap-0.5">
                  <Clock class="w-3 h-3" />{durationOf(item)}
                </span>
              {/if}
            </div>
            <div class="flex items-center gap-1">
              {#if item.priority > 0}
                <span class="badge badge-ghost badge-xs tabular-nums">P{item.priority}</span>
              {/if}
              <QueueItemActions {item} onDownload={downloadNZB} onRetry={retryJob} onRemove={removeFromQueue} />
            </div>
          </div>
        </div>
      {/each}
    </div>

    <!-- Desktop table -->
    <div class="settings-group overflow-x-auto hidden md:block">
      <table class="table table-sm">
        <thead>
          <tr>
            <th class="w-8">
              <input
                type="checkbox"
                class="checkbox checkbox-sm"
                checked={allSelected}
                onclick={toggleSelectAll}
                aria-label={$t("dashboard.file_explorer.select_all")}
              />
            </th>
            <th aria-sort={sortBy === "filename" ? (sortOrder === "asc" ? "ascending" : "descending") : "none"}>
              {@render sortHeader("filename", $t("dashboard.queue.file"), FilenameIcon)}
            </th>
            <th aria-sort={sortBy === "size" ? (sortOrder === "asc" ? "ascending" : "descending") : "none"}>
              {@render sortHeader("size", $t("dashboard.queue.size"), SizeIcon)}
            </th>
            <th aria-sort={sortBy === "status" ? (sortOrder === "asc" ? "ascending" : "descending") : "none"}>
              {@render sortHeader("status", $t("dashboard.queue.status"), StatusSortIcon)}
            </th>
            <th aria-sort={sortBy === "priority" ? (sortOrder === "asc" ? "ascending" : "descending") : "none"}>
              {@render sortHeader("priority", $t("dashboard.queue.priority"), PriorityIcon)}
            </th>
            <th aria-sort={sortBy === "created" ? (sortOrder === "asc" ? "ascending" : "descending") : "none"}>
              {@render sortHeader("created", $t("dashboard.queue.created"), CreatedIcon)}
            </th>
            <th class="text-right">{$t("dashboard.queue.actions")}</th>
          </tr>
        </thead>
        <tbody>
          {#each filteredItems as item (item.id)}
            <tr
              class:row-selected={selectedIds.has(item.id)}
              in:fly
              out:fade={{ speed: "fast" }}
              animate:listFlip
            >
              <td>
                <input
                  type="checkbox"
                  class="checkbox checkbox-sm"
                  checked={selectedIds.has(item.id)}
                  onclick={() => toggleItem(item.id)}
                />
              </td>
              <td>
                <div class="max-w-md">
                  <div class="font-medium truncate" title={item.fileName}>{item.fileName}</div>
                  <div class="text-xs text-base-content/65 truncate" title={item.path}>{item.path}</div>
                </div>
              </td>
              <td class="tabular-nums whitespace-nowrap">{formatFileSize(item.size)}</td>
              <td>
                <div class="flex items-center gap-2 flex-wrap">
                  {#if !(item.status === "complete" && item.verificationStatus === "pending_verification")}
                    <StatusLabel status={item.status} />
                  {/if}
                  <VerificationBadge {item} />
                </div>
                {#if item.errorMessage}
                  <p class="text-xs text-error mt-1 max-w-xs break-words leading-snug">{truncate(item.errorMessage, 120)}</p>
                {/if}
              </td>
              <td>
                {#if item.status === "pending"}
                  <div class="flex items-center gap-0.5">
                    <button
                      class="btn btn-ghost btn-square btn-xs"
                      title={$t("dashboard.queue.increase_priority")}
                      aria-label={$t("dashboard.queue.increase_priority")}
                      onclick={() => changePriority(item.id, item.priority + 1)}
                    ><ArrowUp class="w-3.5 h-3.5" /></button>
                    <span class="text-xs tabular-nums min-w-[1.5rem] text-center">{item.priority}</span>
                    <button
                      class="btn btn-ghost btn-square btn-xs"
                      title={$t("dashboard.queue.decrease_priority")}
                      aria-label={$t("dashboard.queue.decrease_priority")}
                      onclick={() => changePriority(item.id, Math.max(0, item.priority - 1))}
                      disabled={item.priority <= 0}
                    ><ArrowDown class="w-3.5 h-3.5" /></button>
                  </div>
                {:else}
                  <span class="text-sm text-base-content/65 tabular-nums">{item.priority}</span>
                {/if}
              </td>
              <td class="whitespace-nowrap">
                <div class="tabular-nums">{formatDate(item.createdAt)}</div>
                {#if item.completedAt}
                  <div class="text-xs text-base-content/65 tabular-nums">
                    {$t("dashboard.queue.completed")}: {formatDate(item.completedAt)}
                  </div>
                {/if}
                {#if durationOf(item)}
                  <div class="text-xs tabular-nums inline-flex items-center gap-0.5 {item.completedAt ? 'text-base-content/65' : 'text-error/70'}">
                    <Clock class="w-3 h-3" />{durationOf(item)}
                  </div>
                {/if}
              </td>
              <td class="text-right">
                <QueueItemActions {item} onDownload={downloadNZB} onRetry={retryJob} onRemove={removeFromQueue} />
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    {#if totalPages > 1}
      <QueuePagination
        {currentPage}
        {totalPages}
        {totalItems}
        {startItem}
        {endItem}
        {hasNext}
        {hasPrev}
        onPage={goToPage}
      />
    {/if}
  {/if}

  <!-- Batch action bar: floats at the bottom of the viewport while rows are selected -->
  {#if selectedIds.size > 0}
    <div class="batch-bar-wrap" in:fly={{ y: 12 }} out:fly={{ y: 12, speed: "fast" }}>
      <div class="batch-bar">
        <span class="text-sm tabular-nums">{$t("dashboard.file_explorer.items_selected", { values: { count: selectedIds.size } })}</span>
        <button class="btn btn-xs btn-error gap-1" onclick={batchDeleteSelected}>
          <Trash2 class="w-3.5 h-3.5" />
          {$t("dashboard.queue.delete_selected", { values: { count: selectedIds.size } })}
        </button>
        <button
          class="btn btn-xs btn-ghost btn-square"
          onclick={() => (selectedIds = new Set())}
          title={$t("dashboard.file_explorer.clear_selection")}
          aria-label={$t("dashboard.file_explorer.clear_selection")}
        >
          <X class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  {/if}
</section>

{#if pendingBatchDelete}
  <ConfirmDialog
    message={$t("dashboard.queue.confirm_delete_selected", { values: { count: selectedIds.size } })}
    onConfirm={confirmBatchDelete}
    onCancel={() => (pendingBatchDelete = false)}
  />
{/if}

{#if pendingRemoveId}
  <ConfirmDialog
    message={$t("common.messages.confirm_delete")}
    onConfirm={confirmRemove}
    onCancel={cancelRemove}
  />
{/if}

<style>
  tr.row-selected > :global(td),
  div.row-selected {
    background-color: color-mix(in oklab, var(--color-primary) 8%, transparent);
  }
  .batch-bar-wrap {
    position: sticky;
    bottom: 0.75rem;
    z-index: 10;
    display: flex;
    justify-content: center;
    margin-top: 0.75rem;
    pointer-events: none;
  }
  .batch-bar {
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.375rem 0.5rem 0.375rem 0.875rem;
    border-radius: var(--radius-box);
    background: var(--color-base-100);
    box-shadow: var(--ui-shadow-md);
  }
</style>
