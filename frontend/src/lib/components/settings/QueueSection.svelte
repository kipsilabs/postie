<script lang="ts">
import apiClient from "$lib/api/client";
import ByteSizeInput from "$lib/components/inputs/ByteSizeInput.svelte";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import type { config as configType } from "$lib/wailsjs/go/models";
import { Trash2 } from "lucide-svelte";
import FieldRow from "$lib/components/inputs/FieldRow.svelte";
import NumberField from "$lib/components/inputs/NumberField.svelte";
import { fade, scale } from "$lib/motion";
import SettingsSection from "./SettingsSection.svelte";

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

// Reactive local state
let maxConcurrentUploads = $state(config.queue?.max_concurrent_uploads || 3);
let minSizeToStart = $state(config.queue?.min_size_to_start || 0);
let showClearModal = $state(false);
let clearing = $state(false);

const minSizePresets = [
	{ label: $t("settings.queue.min_size_disabled"), value: 0 },
	{ label: "50 GB", value: 50 * 1000 * 1000 * 1000 },
	{ label: "100 GB", value: 100 * 1000 * 1000 * 1000 },
	{ label: "200 GB", value: 200 * 1000 * 1000 * 1000 },
	{ label: "500 GB", value: 500 * 1000 * 1000 * 1000 },
];

// Sync local state back to config
$effect(() => {
	if (!config.queue) {
		config.queue = {
			max_concurrent_uploads: 3,
			min_size_to_start: 0,
		};
	}
	config.queue.max_concurrent_uploads = maxConcurrentUploads;
	config.queue.min_size_to_start = minSizeToStart;
});

async function clearQueue() {
	if (!clearing) {
		try {
			clearing = true;
			await apiClient.clearQueue();
			toastStore.success(
				$t("common.messages.queue_cleared"),
				$t("common.messages.queue_cleared_description")
			);
		} catch (error) {
			console.error("Failed to clear queue:", error);
			toastStore.error(
				$t("common.messages.failed_to_clear_queue"),
				String(error)
			);
		} finally {
			clearing = false;
			showClearModal = false;
		}
	}
}
</script>

<SettingsSection title={$t('settings.queue.title')} footer={$t('settings.queue.info')}>
	<NumberField
		id="max-concurrent"
		bind:value={maxConcurrentUploads}
		label={$t('settings.queue.max_concurrent_uploads')}
		description={$t('settings.queue.max_concurrent_uploads_description')}
		minValue={1}
		maxValue={20}
	/>
	<ByteSizeInput
		bind:value={minSizeToStart}
		id="min-size-to-start"
		label={$t('settings.queue.min_size_to_start')}
		description={$t('settings.queue.min_size_to_start_description')}
		presets={minSizePresets}
		minValue={0}
		maxValue={10 * 1000 * 1000 * 1000 * 1000}
	/>
	<FieldRow label={$t('dashboard.header.clear_completed')}>
		<button
			type="button"
			class="btn btn-sm btn-error btn-outline"
			onclick={() => showClearModal = true}
			disabled={clearing}
		>
			<Trash2 class="w-4 h-4" />
			{$t('dashboard.header.clear_completed')}
		</button>
	</FieldRow>
</SettingsSection>

<!-- Clear Queue Confirmation Modal -->
{#if showClearModal}
	<div class="modal modal-open" role="dialog" aria-modal="true" out:fade={{ speed: "fast" }}>
		<div class="modal-box max-w-md" out:scale={{ speed: "fast", start: 0.97 }}>
			<h3 class="text-base font-semibold mb-2">
				{$t('dashboard.header.clear_completed')}
			</h3>
			<p class="text-sm text-base-content/70">
				Are you sure you want to clear all completed items from the queue? This action cannot be undone.
			</p>
			<div class="modal-action">
				<button
					type="button"
					class="btn btn-sm btn-ghost"
					onclick={() => showClearModal = false}
					disabled={clearing}
				>
					{$t('common.common.cancel')}
				</button>
				<button
					type="button"
					class="btn btn-sm btn-error"
					onclick={clearQueue}
					disabled={clearing}
				>
					<Trash2 class="w-4 h-4" />
					{clearing ? $t('common.common.clearing') : $t('common.common.clear')}
				</button>
			</div>
		</div>
	</div>
{/if}
