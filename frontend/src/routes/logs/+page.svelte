<script lang="ts">
import apiClient from "$lib/api/client";
import { t } from "$lib/i18n";
import { fade } from "$lib/motion";
import { frontendLogs, type LogEntry } from "$lib/stores/logs";
import { toastStore } from "$lib/stores/toast";
import { toolbar } from "$lib/stores/toolbar";
import {
	ArrowDown,
	Copy,
	Download,
	Pause,
	Play,
	RotateCcw,
	Search,
	X,
} from "lucide-svelte";
import { onDestroy, onMount, tick } from "svelte";

type BackendLogEntry = {
	time: string;
	level: "INFO" | "WARN" | "ERROR" | "DEBUG";
	msg: string;
	[key: string]: unknown;
};

type LevelFilter = "all" | "info" | "warn" | "error";

const LEVEL_RANK: Record<LogEntry["level"], number> = {
	debug: 0,
	log: 1,
	info: 1,
	warn: 2,
	error: 3,
};
const FILTER_RANK: Record<LevelFilter, number> = {
	all: 0,
	info: 1,
	warn: 2,
	error: 3,
};
const LEVEL_FILTERS: LevelFilter[] = ["all", "info", "warn", "error"];

let backendLogs = $state<LogEntry[]>([]);
let fLogs = $derived($frontendLogs);

// Pagination state
let loadedLines = 0; // Total number of backend lines we've loaded
let loadingOlder = $state(false);
let hasMoreLogs = $state(true);
const LINES_PER_PAGE = 200;
let scrollDebounceTimer: ReturnType<typeof setTimeout> | undefined;

let combinedLogs = $derived(
	[...backendLogs, ...fLogs].sort(
		(a, b) => a.timestamp.getTime() - b.timestamp.getTime(),
	),
);

let query = $state("");
let levelFilter = $state<LevelFilter>("all");

let visibleLogs = $derived.by(() => {
	const needle = query.trim().toLowerCase();
	const minRank = FILTER_RANK[levelFilter];
	if (!needle && minRank === 0) return combinedLogs;
	return combinedLogs.filter((log) => {
		if (LEVEL_RANK[log.level] < minRank) return false;
		if (!needle) return true;
		return searchText(log).includes(needle);
	});
});

// Stable per-entry keys (by object identity) so prepending older logs never re-keys rows.
const rowKeys = new WeakMap<object, number>();
let nextRowKey = 0;
function rowKey(log: LogEntry): number {
	let key = rowKeys.get(log);
	if (key === undefined) {
		key = nextRowKey++;
		rowKeys.set(log, key);
	}
	return key;
}

let loading = $state(true);
let downloading = $state(false);
let autoRefreshEnabled = $state(false);
let intervalId: ReturnType<typeof setTimeout> | undefined;
let logContainer = $state<HTMLDivElement>();
let autoScroll = $state(true);
let showFollowButton = $state(false);

function searchText(log: LogEntry): string {
	const attrs = log.attributes
		? Object.entries(log.attributes)
				.map(([k, v]) => `${k}=${formatAttributeValue(v)}`)
				.join(" ")
		: "";
	return `${log.level} ${log.message} ${attrs}`.toLowerCase();
}

async function loadInitialLogs() {
	loading = true;
	try {
		// Load the most recent LINES_PER_PAGE lines (offset = 0)
		const rawLogs = await apiClient.getLogsPaginated(LINES_PER_PAGE, 0);
		const parsedLogs = parseLogLines(rawLogs);

		backendLogs = parsedLogs;
		loadedLines = parsedLogs.length;
		hasMoreLogs = parsedLogs.length === LINES_PER_PAGE;
	} catch (error) {
		backendLogs = [
			{
				timestamp: new Date(),
				level: "error",
				message: `Failed to load backend logs: ${error instanceof Error ? error.message : String(error)}`,
			},
		];
		hasMoreLogs = false;
	} finally {
		loading = false;
	}
}

async function loadOlderLogs() {
	if (loadingOlder || !hasMoreLogs) return;

	loadingOlder = true;
	try {
		// Load older logs starting from our current offset
		const rawLogs = await apiClient.getLogsPaginated(
			LINES_PER_PAGE,
			loadedLines,
		);
		const parsedLogs = parseLogLines(rawLogs);

		if (parsedLogs.length === 0) {
			hasMoreLogs = false;
			return;
		}

		// Remember current scroll position relative to the bottom
		const scrollFromBottom = logContainer
			? logContainer.scrollHeight -
				logContainer.scrollTop -
				logContainer.clientHeight
			: 0;

		// Prepend older logs to the beginning
		backendLogs = [...parsedLogs, ...backendLogs];
		loadedLines += parsedLogs.length;
		hasMoreLogs = parsedLogs.length === LINES_PER_PAGE;

		// Restore scroll position after the DOM updates
		requestAnimationFrame(() => {
			if (logContainer) {
				const newScrollTop =
					logContainer.scrollHeight -
					logContainer.clientHeight -
					scrollFromBottom;
				logContainer.scrollTop = Math.max(0, newScrollTop);
			}
		});
	} catch (error) {
		console.error("Failed to load older logs:", error);
		hasMoreLogs = false;
	} finally {
		loadingOlder = false;
	}
}

async function refreshRecentLogs() {
	if (loading) return;

	try {
		// Only refresh the most recent logs (same as initial load)
		const rawLogs = await apiClient.getLogsPaginated(LINES_PER_PAGE, 0);
		const parsedLogs = parseLogLines(rawLogs);

		// Find how many new logs we have
		const oldestCurrentLog =
			backendLogs.length > 0 ? backendLogs[backendLogs.length - 1] : null;
		let newLogsCount = 0;

		if (oldestCurrentLog) {
			// Count how many logs are newer than our newest current log
			newLogsCount = parsedLogs.findIndex(
				(log) =>
					log.timestamp.getTime() <= oldestCurrentLog.timestamp.getTime(),
			);
			if (newLogsCount === -1) {
				newLogsCount = parsedLogs.length;
			}
		} else {
			newLogsCount = parsedLogs.length;
		}

		if (newLogsCount > 0) {
			// Add only the new logs to the end
			const newLogs = parsedLogs.slice(0, newLogsCount);
			backendLogs = [...backendLogs, ...newLogs];
		}
	} catch (error) {
		console.error("Failed to refresh logs:", error);
	}
}

function parseLogLines(rawLogs: string): LogEntry[] {
	if (!rawLogs.trim()) return [];

	return rawLogs
		.split("\n")
		.filter((line) => line.trim() !== "")
		.map((line) => {
			try {
				const entry: BackendLogEntry = JSON.parse(line);
				// Extract custom attributes (everything except time, level, msg)
				const { time, level, msg, ...attributes } = entry;

				return {
					timestamp: new Date(time),
					level: level.toLowerCase() as LogEntry["level"],
					message: msg,
					attributes: Object.keys(attributes).length > 0 ? attributes : undefined,
				};
			} catch (e) {
				return {
					timestamp: new Date(),
					level: "error",
					message: `Failed to parse log line: "${line}"`,
				};
			}
		});
}

function startAutoRefresh() {
	if (autoRefreshEnabled) return;
	autoRefreshEnabled = true;
	const refresh = async () => {
		await refreshRecentLogs();
		if (autoRefreshEnabled) {
			intervalId = setTimeout(refresh, 2000); // 2 seconds
		}
	};
	refresh();
}

function stopAutoRefresh() {
	if (!autoRefreshEnabled) return;
	autoRefreshEnabled = false;
	if (intervalId) {
		clearTimeout(intervalId);
		intervalId = undefined;
	}
}

onMount(() => {
	loadInitialLogs().then(() => {
		startAutoRefresh();
		followLogs();
	});
});

onDestroy(() => {
	stopAutoRefresh();
	if (scrollDebounceTimer) {
		clearTimeout(scrollDebounceTimer);
	}
});

function formatAttributeValue(value: unknown): string {
	if (typeof value === "number") {
		return value.toLocaleString();
	}
	if (typeof value === "boolean") {
		return value ? "true" : "false";
	}
	if (value === null || value === undefined) {
		return "null";
	}
	return String(value);
}

function levelDotClass(level: LogEntry["level"]): string {
	switch (level) {
		case "error":
			return "bg-error";
		case "warn":
			return "bg-warning";
		case "info":
			return "bg-primary";
		case "debug":
			return "bg-base-content/20";
		default:
			return "bg-base-content/35";
	}
}

function formatTime(date: Date): string {
	return date.toLocaleTimeString(undefined, { hour12: false });
}

function handleScroll() {
	if (!logContainer) return;

	const atBottom =
		logContainer.scrollTop + logContainer.clientHeight >=
		logContainer.scrollHeight - 10;
	const atTop = logContainer.scrollTop <= 50; // Increased threshold for better UX

	autoScroll = atBottom;
	showFollowButton = !atBottom;

	// Debounce loading older logs to prevent rapid API calls
	if (atTop && hasMoreLogs && !loadingOlder) {
		if (scrollDebounceTimer) {
			clearTimeout(scrollDebounceTimer);
		}
		scrollDebounceTimer = setTimeout(() => {
			loadOlderLogs();
		}, 100); // 100ms debounce
	}
}

function followLogs() {
	autoScroll = true;
	showFollowButton = false;
	if (logContainer) {
		logContainer.scrollTo({
			top: logContainer.scrollHeight,
			behavior: "instant",
		});
	}
}

function handleRefresh() {
	// Reset pagination and reload from the beginning
	loadedLines = 0;
	hasMoreLogs = true;
	backendLogs = [];
	loadInitialLogs().then(() => {
		followLogs();
	});
}

async function handleDownload() {
	downloading = true;
	try {
		await apiClient.downloadLogFile();
		toastStore.success($t("logs.download_success"), $t("logs.download_success_description"));
	} catch (error) {
		console.error("Failed to download log file:", error);
		toastStore.error($t("logs.download_error"), String(error));
	} finally {
		downloading = false;
	}
}

function serializeLog(log: LogEntry): string {
	const attrs = log.attributes
		? ` ${Object.entries(log.attributes)
				.map(([k, v]) => `${k}=${formatAttributeValue(v)}`)
				.join(" ")}`
		: "";
	return `${log.timestamp.toISOString()} [${log.level.toUpperCase()}] ${log.message}${attrs}`;
}

async function handleCopy() {
	try {
		await navigator.clipboard.writeText(visibleLogs.map(serializeLog).join("\n"));
		toastStore.success($t("logs.copied"));
	} catch (error) {
		console.error("Failed to copy logs:", error);
		toastStore.error($t("logs.copy_error"), String(error));
	}
}

function clearFilters() {
	query = "";
	levelFilter = "all";
}

// Keep the pane pinned to the newest line while following.
$effect(() => {
	visibleLogs.length;
	if (!autoScroll || !logContainer) return;
	const el = logContainer;
	tick().then(() => {
		el.scrollTop = el.scrollHeight;
		showFollowButton = false;
	});
});

$effect(() => {
	toolbar.set({ actions: logActions });
	return () => toolbar.reset();
});

let filtered = $derived(query.trim() !== "" || levelFilter !== "all");
</script>

{#snippet logActions()}
	<div class="flex items-center gap-1">
		<button
			class="btn btn-sm btn-ghost btn-square tooltip tooltip-bottom"
			data-tip={autoRefreshEnabled ? $t("common.common.stop_auto_refresh") : $t("common.common.start_auto_refresh")}
			aria-label={autoRefreshEnabled ? $t("common.common.stop_auto_refresh") : $t("common.common.start_auto_refresh")}
			aria-pressed={autoRefreshEnabled}
			onclick={autoRefreshEnabled ? stopAutoRefresh : startAutoRefresh}
		>
			{#if autoRefreshEnabled}
				<Pause class="h-4 w-4" />
			{:else}
				<Play class="h-4 w-4" />
			{/if}
		</button>
		<button
			class="btn btn-sm btn-ghost btn-square tooltip tooltip-bottom"
			data-tip={$t("common.common.refresh")}
			aria-label={$t("common.common.refresh")}
			onclick={handleRefresh}
			disabled={loading || autoRefreshEnabled}
		>
			<RotateCcw class="h-4 w-4" />
		</button>
		<button
			class="btn btn-sm btn-ghost btn-square tooltip tooltip-bottom"
			data-tip={$t("logs.copy")}
			aria-label={$t("logs.copy")}
			onclick={handleCopy}
			disabled={visibleLogs.length === 0}
		>
			<Copy class="h-4 w-4" />
		</button>
		<button
			class="btn btn-sm btn-ghost btn-square tooltip tooltip-bottom"
			data-tip={$t("logs.download")}
			aria-label={$t("logs.download")}
			onclick={handleDownload}
			disabled={downloading}
		>
			{#if downloading}
				<span class="loading loading-spinner loading-xs"></span>
			{:else}
				<Download class="h-4 w-4" />
			{/if}
		</button>
	</div>
{/snippet}

<div class="logs-page">
	<!-- Compact filter bar -->
	<div class="flex flex-wrap items-center gap-2">
		<label class="input input-sm flex min-w-0 flex-1 basis-48 items-center gap-2">
			<Search class="h-4 w-4 shrink-0 opacity-60" />
			<input
				type="search"
				class="min-w-0 grow"
				placeholder={$t("logs.search_placeholder")}
				aria-label={$t("logs.search_placeholder")}
				bind:value={query}
			/>
			{#if query}
				<button
					type="button"
					class="shrink-0 opacity-60 hover:opacity-100"
					aria-label={$t("common.actions.close")}
					onclick={() => (query = "")}
				>
					<X class="h-3.5 w-3.5" />
				</button>
			{/if}
		</label>
		<select
			class="select select-sm w-auto"
			aria-label={$t("logs.level_label")}
			bind:value={levelFilter}
		>
			{#each LEVEL_FILTERS as option (option)}
				<option value={option}>{$t(`logs.level_${option}`)}</option>
			{/each}
		</select>
		<span class="ml-auto text-xs tabular-nums text-base-content/60" aria-live="off">
			{#if filtered}
				<button type="button" class="link link-hover" onclick={clearFilters}>
					{$t("logs.count_filtered", { values: { shown: visibleLogs.length, total: combinedLogs.length } })}
				</button>
			{:else}
				{$t("logs.count", { values: { count: combinedLogs.length } })}
			{/if}
			{#if hasMoreLogs}· {$t("logs.more_available")}{/if}
		</span>
	</div>

	<!-- Log pane -->
	<div class="log-pane">
		{#if showFollowButton}
			<button
				class="btn btn-sm btn-primary jump-latest"
				transition:fade={{ speed: "fast" }}
				onclick={followLogs}
			>
				<ArrowDown class="h-4 w-4" />
				{$t("logs.jump_to_latest")}
			</button>
		{/if}

		{#if loadingOlder}
			<div class="loading-older badge badge-neutral gap-2" transition:fade={{ speed: "fast" }}>
				<span class="loading loading-spinner loading-xs"></span>
				{$t("logs.loading_older")}
			</div>
		{/if}

		<div
			bind:this={logContainer}
			class="log-scroll selectable"
			role="log"
			aria-label={$t("common.nav.logs")}
			onscroll={handleScroll}
		>
			{#if !hasMoreLogs && backendLogs.length > 0}
				<div class="log-edge">{$t("logs.beginning")}</div>
			{/if}

			{#each visibleLogs as log, i (rowKey(log))}
				<div class="log-row" class:animate-fade-in={i === visibleLogs.length - 1 && !loading}>
					<span class="log-time">{formatTime(log.timestamp)}</span>
					<span
						class="log-dot {levelDotClass(log.level)}"
						title={log.level.toUpperCase()}
						role="img"
						aria-label={log.level}
					></span>
					<span class="log-msg" class:text-error={log.level === "error"}>
						{log.message}
						{#if log.attributes && Object.keys(log.attributes).length > 0}
							{#each Object.entries(log.attributes) as [key, value] (key)}
								<span class="log-attr"><span class="log-attr-key">{key}</span>={formatAttributeValue(value)}</span>
							{/each}
						{/if}
					</span>
				</div>
			{/each}

			{#if loading}
				<div class="log-state">
					<span class="loading loading-spinner loading-sm"></span>
					{$t("logs.loading")}
				</div>
			{:else if visibleLogs.length === 0}
				<div class="log-state">{$t("logs.empty")}</div>
			{/if}
		</div>
	</div>
</div>

<style>
	.logs-page {
		display: flex;
		flex-direction: column;
		gap: 0.625rem;
		/* Fill the content pane: window minus toolbar and the shell page padding. */
		height: max(18rem, calc(100dvh - var(--toolbar-height) - 3.25rem));
		min-width: 0;
	}
	.log-pane {
		position: relative;
		flex: 1;
		min-height: 0;
		overflow: hidden;
		border-radius: var(--radius-box);
		background: var(--color-base-200);
		box-shadow: 0 0 0 1px var(--ui-hairline);
	}
	.log-scroll {
		height: 100%;
		overflow-y: auto;
		overflow-x: hidden;
		overscroll-behavior: contain;
		padding: 0.25rem 0;
		font-family: var(--ui-mono);
		font-size: 0.75rem;
		line-height: 1.1rem;
	}
	.log-row {
		display: grid;
		grid-template-columns: 4.5rem 0.5rem minmax(0, 1fr);
		align-items: baseline;
		column-gap: 0.625rem;
		padding: 0.0625rem 0.75rem;
	}
	.log-row:hover {
		background: color-mix(in oklab, var(--color-base-content) 5%, transparent);
	}
	.log-time {
		color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.log-dot {
		width: 0.4375rem;
		height: 0.4375rem;
		border-radius: 999px;
		align-self: center;
	}
	.log-msg {
		min-width: 0;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.log-attr {
		margin-left: 0.5rem;
		color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
	}
	.log-attr-key {
		color: color-mix(in oklab, var(--color-base-content) 75%, transparent);
	}
	.log-edge,
	.log-state {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		padding: 0.75rem;
		font-family: var(--ui-font);
		color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
	}
	.jump-latest {
		position: absolute;
		z-index: 10;
		bottom: 0.75rem;
		left: 50%;
		translate: -50% 0;
		box-shadow: var(--ui-shadow-md);
	}
	.loading-older {
		position: absolute;
		z-index: 10;
		top: 0.5rem;
		left: 50%;
		translate: -50% 0;
	}
</style>
