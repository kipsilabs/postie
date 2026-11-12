<script lang="ts">
import DurationInput from "$lib/components/inputs/DurationInput.svelte";
import { t } from "$lib/i18n";
import type { config as configType } from "$lib/wailsjs/go/models";
import NumberField from "$lib/components/inputs/NumberField.svelte";
import ToggleRow from "$lib/components/inputs/ToggleRow.svelte";
import SettingsSection from "./SettingsSection.svelte";
import { collapse } from "./collapse";

const presets = [
	{ label: "5s", value: 5, unit: "s" },
	{ label: "10s", value: 10, unit: "s" },
	{ label: "30s", value: 30, unit: "s" },
	{ label: "1m", value: 1, unit: "m" },
];

const deferredDelayPresets = [
	{ label: "30s", value: 30, unit: "s" },
	{ label: "1m", value: 1, unit: "m" },
	{ label: "5m", value: 5, unit: "m" },
	{ label: "10m", value: 10, unit: "m" },
];

const deferredBackoffPresets = [
	{ label: "5m", value: 5, unit: "m" },
	{ label: "15m", value: 15, unit: "m" },
	{ label: "30m", value: 30, unit: "m" },
	{ label: "1h", value: 1, unit: "h" },
];

const deferredIntervalPresets = [
	{ label: "1m", value: 1, unit: "m" },
	{ label: "2m", value: 2, unit: "m" },
	{ label: "5m", value: 5, unit: "m" },
];

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

// Reactive local state
let enabled = $state(config.post_check?.enabled ?? true);
let delay = $state(config.post_check?.delay || "10s");
let maxReposts = $state(config.post_check?.max_reposts || 1);
let deferredCheckDelay = $state(config.post_check?.deferred_check_delay || "30s");
let deferredMaxRetries = $state(config.post_check?.deferred_max_retries || 5);
let deferredMaxBackoff = $state(config.post_check?.deferred_max_backoff || "5m");
let deferredCheckInterval = $state(config.post_check?.deferred_check_interval || "2m");
let deferredBatchSize = $state(config.post_check?.deferred_batch_size || 10000);
let statBatchSize = $state(config.post_check?.stat_batch_size || 100);
let maxConcurrentChecks = $state(config.post_check?.max_concurrent_checks ?? 0);

// Ensure post_check exists with defaults
if (!config.post_check) {
	config.post_check = {
		enabled: true,
		delay: "10s",
		max_reposts: 1,
		deferred_check_delay: "30s",
		deferred_max_retries: 5,
		deferred_max_backoff: "5m",
		deferred_check_interval: "2m",
		deferred_batch_size: 10000,
		stat_batch_size: 100,
		max_concurrent_checks: 0,
	};
}

// Sync local state back to config
$effect(() => {
	config.post_check.enabled = enabled;
});

$effect(() => {
	config.post_check.delay = delay;
});

$effect(() => {
	config.post_check.max_reposts = maxReposts;
});

$effect(() => {
	config.post_check.deferred_check_delay = deferredCheckDelay;
});

$effect(() => {
	config.post_check.deferred_max_retries = deferredMaxRetries;
});

$effect(() => {
	config.post_check.deferred_max_backoff = deferredMaxBackoff;
});

$effect(() => {
	config.post_check.deferred_check_interval = deferredCheckInterval;
});

$effect(() => {
	config.post_check.deferred_batch_size = deferredBatchSize;
});

$effect(() => {
	config.post_check.stat_batch_size = statBatchSize;
});

$effect(() => {
	config.post_check.max_concurrent_checks = maxConcurrentChecks;
});

</script>

<SettingsSection
	title={$t('settings.post_check.title')}
	footer={`${$t('settings.post_check.info_title')} ${$t('settings.post_check.info_description')}`}
>
	<ToggleRow
		id="post-check-enabled"
		bind:checked={enabled}
		label={$t('settings.post_check.enable')}
		description={$t('settings.post_check.enable_description')}
	/>

	{#if enabled}
		<div transition:collapse class="border-t border-(--ui-hairline)">
			<DurationInput
				id="check-delay"
				bind:value={delay}
				label={$t('settings.post_check.check_delay')}
				description={$t('settings.post_check.check_delay_description')}
				placeholder="10"
				minValue={1}
				maxValue={300}
				presets={presets}
			/>
			<NumberField
				id="max-reposts"
				bind:value={maxReposts}
				label={$t('settings.post_check.max_reposts')}
				description={$t('settings.post_check.max_reposts_description')}
				minValue={0}
				maxValue={10}
			/>
		</div>
	{/if}
</SettingsSection>

{#if enabled}
	<div transition:collapse>
		<div>
			<SettingsSection
				title={$t('settings.post_check.deferred_title')}
				description={$t('settings.post_check.deferred_info')}
			>
				<DurationInput
					id="deferred-check-delay"
					bind:value={deferredCheckDelay}
					label={$t('settings.post_check.deferred_check_delay')}
					description={$t('settings.post_check.deferred_check_delay_description')}
					placeholder="5m"
					minValue={1}
					maxValue={60}
					presets={deferredDelayPresets}
				/>
				<NumberField
					id="deferred-max-retries"
					bind:value={deferredMaxRetries}
					label={$t('settings.post_check.deferred_max_retries')}
					description={$t('settings.post_check.deferred_max_retries_description')}
					minValue={1}
					maxValue={20}
				/>
				<DurationInput
					id="deferred-max-backoff"
					bind:value={deferredMaxBackoff}
					label={$t('settings.post_check.deferred_max_backoff')}
					description={$t('settings.post_check.deferred_max_backoff_description')}
					placeholder="1h"
					minValue={1}
					maxValue={24}
					presets={deferredBackoffPresets}
				/>
				<DurationInput
					id="deferred-check-interval"
					bind:value={deferredCheckInterval}
					label={$t('settings.post_check.deferred_check_interval')}
					description={$t('settings.post_check.deferred_check_interval_description')}
					placeholder="2m"
					minValue={1}
					maxValue={30}
					presets={deferredIntervalPresets}
				/>
				<NumberField
					id="deferred-batch-size"
					bind:value={deferredBatchSize}
					label={$t('settings.post_check.deferred_batch_size')}
					description={$t('settings.post_check.deferred_batch_size_description')}
					minValue={1}
					maxValue={10000}
				/>
				<NumberField
					id="stat-batch-size"
					bind:value={statBatchSize}
					label={$t('settings.post_check.stat_batch_size')}
					description={$t('settings.post_check.stat_batch_size_description')}
					minValue={1}
					maxValue={1000}
				/>
				<NumberField
					id="max-concurrent-checks"
					bind:value={maxConcurrentChecks}
					label={$t('settings.post_check.max_concurrent_checks')}
					description={$t('settings.post_check.max_concurrent_checks_description')}
					minValue={0}
					maxValue={64}
				/>
			</SettingsSection>
		</div>
	</div>
{/if}
