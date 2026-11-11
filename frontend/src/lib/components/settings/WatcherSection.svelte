<script lang="ts">
import apiClient from "$lib/api/client";
import DurationInput from "$lib/components/inputs/DurationInput.svelte";
import SizeInput from "$lib/components/inputs/SizeInput.svelte";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import { config as configType } from "$lib/wailsjs/go/models";
import { ChevronDown, CirclePlus, FolderOpen, Trash2 } from "lucide-svelte";
import FieldRow from "$lib/components/inputs/FieldRow.svelte";
import ToggleRow from "$lib/components/inputs/ToggleRow.svelte";
import { fly } from "$lib/motion";
import { collapse } from "./collapse";

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

// Ensure watchers array is initialized
$effect(() => {
	if (!config.watchers || config.watchers.length === 0) {
		config.watchers = [createDefaultWatcher()];
	}
});

// Track expanded state per watcher card
let expanded = $state<boolean[]>([]);
$effect(() => {
	// Grow expanded array to match watchers length, default new ones to true
	const watchers = config.watchers || [];
	while (expanded.length < watchers.length) {
		expanded.push(true);
	}
});

// Preset definitions
const checkIntervalPresets = [
  { label: "30s", value: 30, unit: "s" },
  { label: "2m", value: 2, unit: "m" },
  { label: "5m", value: 5, unit: "m" },
  { label: "10m", value: 10, unit: "m" },
];

const minFileAgePresets = [
  { label: "30s", value: 30, unit: "s" },
  { label: "1m", value: 1, unit: "m" },
  { label: "5m", value: 5, unit: "m" },
  { label: "10m", value: 10, unit: "m" },
];

const minFileAgeToDeletePresets = [
  { label: "0s", value: 0, unit: "s" },
  { label: "5m", value: 5, unit: "m" },
  { label: "10m", value: 10, unit: "m" },
  { label: "30m", value: 30, unit: "m" },
];

const sizeThresholdPresets = [
  { label: "50MB", value: 50, unit: "MB" },
  { label: "100MB", value: 100, unit: "MB" },
  { label: "500MB", value: 500, unit: "MB" },
  { label: "1GB", value: 1, unit: "GB" },
];

const minFileSizePresets = [
  { label: "0B", value: 0, unit: "B" },
  { label: "1MB", value: 1, unit: "MB" },
  { label: "10MB", value: 10, unit: "MB" },
  { label: "50MB", value: 50, unit: "MB" },
  { label: "100MB", value: 100, unit: "MB" },
];

function createDefaultWatcher(): configType.WatcherConfig {
	const w = new configType.WatcherConfig();
	w.name = "";
	w.enabled = false;
	w.watch_directory = "";
	w.check_interval = "5m";
	w.min_file_age = "60s";
	w.min_file_age_to_delete = "0s";
	w.size_threshold = 104857600;
	w.min_file_size = 1048576;
	w.delete_original_file = false;
	w.single_nzb_per_folder = false;
	w.follow_symlinks = false;
	w.schedule = { start_time: "00:00", end_time: "23:59" };
	w.ignore_patterns = [];
	return w;
}

function addWatcher() {
	config.watchers = [...(config.watchers || []), createDefaultWatcher()];
	expanded = [...expanded, true];
}

function removeWatcher(index: number) {
	if ((config.watchers || []).length <= 1) return;
	config.watchers = (config.watchers || []).filter((_, i) => i !== index);
	expanded = expanded.filter((_, i) => i !== index);
}

function toggleExpanded(index: number) {
	expanded[index] = !expanded[index];
}

async function selectWatchDirectory(index: number) {
	try {
		await apiClient.initialize();

		if (apiClient.environment !== "wails") {
			toastStore.warning($t("common.messages.wails_only_feature"));
			return;
		}

		const { SelectWatchDirectory } = await import("$lib/wailsjs/go/backend/App");
		const dir = await SelectWatchDirectory();

		if (dir) {
			config.watchers[index].watch_directory = dir;
		}
	} catch (error) {
		console.error("Failed to select directory:", error);
		toastStore.error($t("common.messages.error_selecting_directory"), String(error));
	}
}

function addIgnorePattern(index: number) {
	const w = config.watchers[index];
	w.ignore_patterns = [...(w.ignore_patterns || []), ""];
	config.watchers[index] = w;
}

function removeIgnorePattern(watcherIndex: number, patternIndex: number) {
	const w = config.watchers[watcherIndex];
	w.ignore_patterns = (w.ignore_patterns || []).filter((_, i) => i !== patternIndex);
	config.watchers[watcherIndex] = w;
}

function handlePatternInput(watcherIndex: number, patternIndex: number, value: string) {
	const w = config.watchers[watcherIndex];
	w.ignore_patterns = (w.ignore_patterns || []).map((p, i) => i === patternIndex ? value : p);
	config.watchers[watcherIndex] = w;
}

function getWatcherDisplayName(w: configType.WatcherConfig, index: number): string {
	if (w.name) return w.name;
	return $t("settings.watcher.watcher_number", { values: { n: index + 1 } });
}

</script>

<div class="space-y-4">
	<div class="flex items-end justify-between gap-3">
		<div class="section-label !mb-0">{$t('settings.watcher.title')}</div>
		<button type="button" class="btn btn-sm" onclick={addWatcher}>
			<CirclePlus class="w-4 h-4" />
			{$t('settings.watcher.add_directory')}
		</button>
	</div>
	<p class="settings-row-desc mx-1 -mt-2">{$t('settings.watcher.description')}</p>

	{#each (config.watchers || []) as watcher, index (index)}
		<div class="settings-group" in:fly={{ y: 8, speed: "fast" }}>
			<!-- Header -->
			<div
				class="settings-row cursor-pointer hover:bg-base-200/60 transition-colors"
				role="button"
				tabindex="0"
				aria-expanded={!!expanded[index]}
				onclick={() => toggleExpanded(index)}
				onkeydown={(e) => e.key === 'Enter' && toggleExpanded(index)}
			>
				<input
					type="checkbox"
					class="toggle toggle-primary toggle-sm"
					aria-label={getWatcherDisplayName(watcher, index)}
					bind:checked={watcher.enabled}
					onclick={(e) => e.stopPropagation()}
				/>
				<div class="flex-1 min-w-0">
					{#if watcher.name}
						<span class="settings-row-title">{watcher.name}</span>
					{:else}
						<span class="text-base-content/60 text-sm">
							{$t('settings.watcher.watcher_number', { values: { n: index + 1 } })}
						</span>
					{/if}
					{#if watcher.watch_directory}
						<p class="settings-row-desc truncate">{watcher.watch_directory}</p>
					{/if}
				</div>
				<div class="flex items-center gap-1">
					{#if (config.watchers || []).length > 1}
						<button
							type="button"
							class="btn btn-xs btn-ghost text-error"
							onclick={(e) => { e.stopPropagation(); removeWatcher(index); }}
							title={$t('settings.watcher.remove_directory')}
							aria-label={$t('settings.watcher.remove_directory')}
						>
							<Trash2 class="w-4 h-4" />
						</button>
					{/if}
					<ChevronDown
						class="w-4 h-4 text-base-content/60 transition-transform duration-(--motion-base) {expanded[index] ? 'rotate-180' : ''}"
					/>
				</div>
			</div>

			<!-- Body (collapsible) -->
			{#if expanded[index]}
				<div transition:collapse class="border-t border-(--ui-hairline)">
					<FieldRow
						label={$t('settings.watcher.watcher_name')}
						description={$t('settings.watcher.watcher_name_description')}
						id="watcher-name-{index}"
					>
						<input
							id="watcher-name-{index}"
							type="text"
							class="input input-sm w-64 max-sm:w-full"
							bind:value={watcher.name}
							placeholder={$t('settings.watcher.watcher_number', { values: { n: index + 1 } })}
						/>
					</FieldRow>

					<FieldRow
						label={$t('settings.watcher.watch_directory')}
						description={$t('settings.watcher.watch_directory_description')}
						id="watch-directory-{index}"
					>
						{#snippet note()}
							{#if apiClient.environment === 'web'}
								<div class="settings-row-desc">Enter the container path directly (e.g., /app/watch)</div>
							{/if}
						{/snippet}
						<div class="flex items-center gap-2 max-sm:flex-col max-sm:items-stretch">
							{#if apiClient.environment === 'wails'}
								<input
									id="watch-directory-{index}"
									class="input input-sm w-64 max-sm:w-full"
									value={watcher.watch_directory}
									readonly
									placeholder={$t('common.inputs.select_directory')}
								/>
								<button type="button" class="btn btn-sm" onclick={() => selectWatchDirectory(index)}>
									<FolderOpen class="w-4 h-4" />
									{$t('common.inputs.browse')}
								</button>
							{:else}
								<input
									id="watch-directory-{index}"
									class="input input-sm w-64 max-sm:w-full"
									bind:value={watcher.watch_directory}
									placeholder="/path/to/watch/directory"
								/>
							{/if}
						</div>
					</FieldRow>

					<DurationInput
						bind:value={watcher.check_interval}
						label={$t('settings.watcher.check_interval')}
						description={$t('settings.watcher.check_interval_description')}
						presets={checkIntervalPresets}
						id="check-interval-{index}"
					/>
					<DurationInput
						bind:value={watcher.min_file_age}
						label={$t('settings.watcher.min_file_age')}
						description={$t('settings.watcher.min_file_age_description')}
						presets={minFileAgePresets}
						id="min-file-age-{index}"
					/>
					<DurationInput
						bind:value={watcher.min_file_age_to_delete}
						label={$t('settings.watcher.min_file_age_to_delete')}
						description={$t('settings.watcher.min_file_age_to_delete_description')}
						presets={minFileAgeToDeletePresets}
						id="min-file-age-to-delete-{index}"
					/>
					<SizeInput
						bind:value={watcher.size_threshold}
						label={$t('settings.watcher.size_threshold')}
						description={$t('settings.watcher.size_threshold_description')}
						presets={sizeThresholdPresets}
						minValue={1}
						id="size-threshold-{index}"
					/>
					<SizeInput
						bind:value={watcher.min_file_size}
						label={$t('settings.watcher.min_file_size')}
						description={$t('settings.watcher.min_file_size_description')}
						presets={minFileSizePresets}
						minValue={0}
						id="min-file-size-{index}"
					/>

					<ToggleRow
						id="delete-original-file-{index}"
						bind:checked={watcher.delete_original_file}
						label={$t('settings.watcher.delete_original_file')}
						description={$t('settings.watcher.delete_original_file_description')}
					/>
					<ToggleRow
						id="single-nzb-per-folder-{index}"
						bind:checked={watcher.single_nzb_per_folder}
						label={$t('settings.watcher.single_nzb_per_folder')}
						description={$t('settings.watcher.single_nzb_per_folder_description')}
					/>

					<FieldRow
						label={$t('settings.watcher.posting_schedule')}
						description={$t('settings.watcher.posting_schedule_description')}
					>
						<div class="flex items-center gap-2">
							<label class="sr-only" for="start-time-{index}">{$t('settings.watcher.start_time')}</label>
							<input id="start-time-{index}" type="time" class="input input-sm" bind:value={watcher.schedule.start_time} />
							<span class="text-base-content/50">–</span>
							<label class="sr-only" for="end-time-{index}">{$t('settings.watcher.end_time')}</label>
							<input id="end-time-{index}" type="time" class="input input-sm" bind:value={watcher.schedule.end_time} />
						</div>
					</FieldRow>

					<FieldRow
						label={$t('settings.watcher.ignore_patterns')}
						description={$t('settings.watcher.ignore_patterns_description')}
					>
						<button type="button" class="btn btn-sm" onclick={() => addIgnorePattern(index)}>
							<CirclePlus class="w-4 h-4" />
							{$t('settings.watcher.add_pattern')}
						</button>
					</FieldRow>
					<div class="settings-row flex-col items-stretch gap-2 border-t border-(--ui-hairline)">
						{#each (watcher.ignore_patterns || []) as pattern, pi (pi)}
							<div class="flex items-center gap-2" in:fly={{ y: 6, speed: "fast" }}>
								<input
									class="input input-sm flex-1 font-mono"
									value={pattern}
									placeholder={$t('settings.watcher.pattern_placeholder')}
									oninput={(e) => handlePatternInput(index, pi, e.currentTarget.value)}
								/>
								<button type="button" class="btn btn-sm btn-ghost text-error" onclick={() => removeIgnorePattern(index, pi)}>
									<Trash2 class="w-4 h-4" />
									{$t('settings.watcher.remove')}
								</button>
							</div>
						{/each}
						<p class="settings-row-desc">{@html $t('settings.watcher.common_patterns')}</p>
					</div>
				</div>
			{/if}
		</div>
	{/each}
</div>
