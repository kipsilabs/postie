<script lang="ts">
// Imports
import DurationInput from "$lib/components/inputs/DurationInput.svelte";
import ThrottleRateInput from "$lib/components/inputs/ThrottleRateInput.svelte";
import { t } from "$lib/i18n";
import { advancedMode } from "$lib/stores/app";
import { config as configType } from "$lib/wailsjs/go/models";
import { Plus, Trash2 } from "lucide-svelte";
import FieldRow from "$lib/components/inputs/FieldRow.svelte";
import NumberField from "$lib/components/inputs/NumberField.svelte";
import ToggleRow from "$lib/components/inputs/ToggleRow.svelte";
import { fly } from "$lib/motion";
import SettingsSection from "./SettingsSection.svelte";
import { collapse } from "./collapse";
  import SizeInput from "$lib/components/inputs/SizeInput.svelte";

// Types
interface ComponentProps {
	config: configType.ConfigData;
}

// Props
let { config = $bindable() }: ComponentProps = $props();

// Reactive local state for form inputs (following best practices)
let maxRetries = $state(config.posting.max_retries || 3);
let retryDelay = $state(config.posting.retry_delay || "5s");
let articleSizeInBytes = $state(config.posting.article_size_in_bytes || 750000);
let waitForPar2 = $state(config.posting.wait_for_par2 ?? true);
let groups = $state(
	config.posting.groups?.length > 0 
		? [...config.posting.groups] 
		: [{ name: "alt.binaries.test", enabled: true }],
);
let customHeaders = $state(
	config.posting.post_headers?.custom_headers
		? [...config.posting.post_headers.custom_headers]
		: [],
);
let obfuscationPolicy = $state(config.posting.obfuscation_policy || "full");
let par2ObfuscationPolicy = $state(
	config.posting.par2_obfuscation_policy || "full",
);
let messageIdFormat = $state(config.posting.message_id_format || "random");
let groupPolicy = $state(config.posting.group_policy || "each_file");
let defaultFrom = $state(config.posting.post_headers?.default_from || "");
let addNgxHeader = $state(config.posting.post_headers?.add_nxg_header ?? false);
let throttleRateMB = $state(
	Math.round((config.posting.throttle_rate || 0) / 1048576),
);
let uploadBufferMemoryLimit = $state(config.posting.upload_buffer_memory_limit ?? 0);

// Initialize defaults (following initialization pattern)
function initializeDefaults() {
	if (!config.posting.post_headers) {
		config.posting.post_headers = new configType.PostHeaders();
	}

	// Set other defaults
	config.posting.throttle_rate = config.posting.throttle_rate || 0;
	config.posting.message_id_format =
		config.posting.message_id_format || "random";
	config.posting.par2_obfuscation_policy =
		config.posting.par2_obfuscation_policy || "full";
	config.posting.group_policy = config.posting.group_policy || "each_file";
}

// Initialize on component load
initializeDefaults();

// Effects - Sync local state back to config (consolidated pattern)
$effect(() => {
	// Sync all posting values in one effect for better performance
	config.posting.max_retries = maxRetries;
	config.posting.retry_delay = retryDelay;
	config.posting.article_size_in_bytes = articleSizeInBytes;
	config.posting.wait_for_par2 = waitForPar2;
	config.posting.groups = [...groups];
	config.posting.obfuscation_policy = obfuscationPolicy;
	config.posting.par2_obfuscation_policy = par2ObfuscationPolicy;
	config.posting.message_id_format = messageIdFormat;
	config.posting.group_policy = groupPolicy;
	config.posting.throttle_rate = throttleRateMB * 1048576;
	config.posting.upload_buffer_memory_limit = uploadBufferMemoryLimit;
});

$effect(() => {
	// Sync post headers separately to handle initialization
	if (!config.posting.post_headers) {
		config.posting.post_headers = new configType.PostHeaders();
	}
	config.posting.post_headers.default_from = defaultFrom;
	config.posting.post_headers.add_nxg_header = addNgxHeader;
	config.posting.post_headers.custom_headers = [...customHeaders];
});

// Ensure at least one group exists
$effect(() => {
	if (groups.length === 0) {
		groups = [{ name: "alt.binaries.test", enabled: true }];
	}
});

// Derived state - reactive arrays for dropdowns
const obfuscationOptions = $derived([
	{ value: "none", name: $t("settings.posting.obfuscation.none") },
	{ value: "partial", name: $t("settings.posting.obfuscation.partial") },
	{ value: "full", name: $t("settings.posting.obfuscation.full") },
]);

const messageIdOptions = $derived([
	{ value: "random", name: $t("settings.posting.message_id.random") },
	{ value: "nxg", name: $t("settings.posting.message_id.nxg") },
]);

const groupPolicyOptions = $derived([
	{ value: "all", name: $t("settings.posting.group_policy_options.all") },
	{
		value: "each_file",
		name: $t("settings.posting.group_policy_options.each_file"),
	},
]);

// Preset definitions (static data)
const retryDelayPresets = [
	{ label: "5s", value: 5, unit: "s" },
	{ label: "30s", value: 30, unit: "s" },
	{ label: "1m", value: 1, unit: "m" },
	{ label: "5m", value: 5, unit: "m" },
];

const articleSizePresets = [
	{ label: "500KB", value: 500, unit: "KB" },
	{ label: "750KB", value: 750, unit: "KB" },
	{ label: "1MB", value: 1, unit: "MB" },
];

const throttleRatePresets = [
	{ label: "Unlimited", value: 0 },
	{ label: "5 MB/s", value: 5 },
	{ label: "10 MB/s", value: 10 },
	{ label: "25 MB/s", value: 25 },
	{ label: "50 MB/s", value: 50 },
];

// Functions
function addGroup() {
	groups = [...groups, { name: "", enabled: true }];
}

function removeGroup(index: number) {
	groups = groups.filter((_, i) => i !== index);
}

function addCustomHeader() {
	customHeaders = [...customHeaders, { name: "", value: "" }];
}

function removeCustomHeader(index: number) {
	customHeaders = customHeaders.filter((_, i) => i !== index);
}

// ThrottleRate update function (removed as it's now handled in $effect)
function updateThrottleRate() {
	// This function is kept for ThrottleRateInput component compatibility
	// Actual sync happens in $effect above
}

</script>

<div class="space-y-6">
	<SettingsSection title={$t('settings.posting.title')}>
		<NumberField
			id="max-retries"
			bind:value={maxRetries}
			label={$t('settings.posting.max_retries')}
			description={$t('settings.posting.max_retries_description')}
			minValue={0}
			maxValue={10}
		/>
		<DurationInput
			bind:value={retryDelay}
			label={$t('settings.posting.retry_delay')}
			description={$t('settings.posting.retry_delay_description')}
			presets={retryDelayPresets}
			id="retry-delay"
		/>
		<SizeInput
			bind:value={articleSizeInBytes}
			label={$t('settings.posting.article_size')}
			description={$t('settings.posting.article_size_description')}
			presets={articleSizePresets}
			minValue={1000}
			maxValue={10000000}
			id="article-size"
		/>

		{#if $advancedMode}
			<div transition:collapse class="border-t border-(--ui-hairline)">
				<ThrottleRateInput
					bind:value={throttleRateMB}
					onchange={updateThrottleRate}
					label={$t('settings.posting.throttle_rate')}
					description={$t('settings.posting.throttle_rate_description')}
					presets={throttleRatePresets}
					id="throttle-rate"
				/>
				<SizeInput
					bind:value={uploadBufferMemoryLimit}
					label={$t('settings.posting.upload_buffer_memory_limit')}
					description={$t('settings.posting.upload_buffer_memory_limit_description')}
					minValue={0}
					id="upload-buffer-memory-limit"
				/>
				<FieldRow
					label={$t('settings.posting.obfuscation_policy')}
					description={obfuscationPolicy === 'none' || obfuscationPolicy === 'partial' || obfuscationPolicy === 'full' ? $t(`settings.posting.obfuscation.${obfuscationPolicy}_description`) : ''}
					html
					id="obfuscation"
				>
					<select id="obfuscation" class="select select-sm w-auto min-w-40" bind:value={obfuscationPolicy}>
						{#each obfuscationOptions as option}
							<option value={option.value}>{option.name}</option>
						{/each}
					</select>
				</FieldRow>
				<FieldRow
					label={$t('settings.posting.par2_obfuscation_policy')}
					description={par2ObfuscationPolicy === 'none' || par2ObfuscationPolicy === 'partial' || par2ObfuscationPolicy === 'full' ? $t(`settings.posting.par2_obfuscation.${par2ObfuscationPolicy}_description`) : ''}
					html
					id="par2-obfuscation"
				>
					<select id="par2-obfuscation" class="select select-sm w-auto min-w-40" bind:value={par2ObfuscationPolicy}>
						{#each obfuscationOptions as option}
							<option value={option.value}>{option.name}</option>
						{/each}
					</select>
				</FieldRow>
				<FieldRow
					label={$t('settings.posting.message_id_format')}
					description={$t('settings.posting.message_id_format_description')}
					id="message-id-format"
				>
					<select id="message-id-format" class="select select-sm w-auto min-w-40" bind:value={messageIdFormat}>
						{#each messageIdOptions as option}
							<option value={option.value}>{option.name}</option>
						{/each}
					</select>
				</FieldRow>
				<FieldRow
					label={$t('settings.posting.group_policy')}
					description={$t('settings.posting.group_policy_description')}
					id="group-policy"
				>
					<select id="group-policy" class="select select-sm w-auto min-w-40" bind:value={groupPolicy}>
						{#each groupPolicyOptions as option}
							<option value={option.value}>{option.name}</option>
						{/each}
					</select>
				</FieldRow>
			</div>
		{/if}

		<ToggleRow
			id="waitforpar2"
			bind:checked={waitForPar2}
			label={$t('settings.posting.wait_for_par2')}
			description={$t('settings.posting.wait_for_par2_description')}
		/>
	</SettingsSection>

	{#if $advancedMode}
		<div transition:collapse>
			<SettingsSection title={$t('settings.posting.headers.title')}>
				<FieldRow
					label={$t('settings.posting.headers.default_from')}
					description={$t('settings.posting.headers.default_from_description')}
					id="default-from"
				>
					<input
						id="default-from"
						class="input input-sm w-72 max-sm:w-full"
						bind:value={defaultFrom}
						placeholder="Poster <poster@example.com>"
						type="text"
					/>
				</FieldRow>
				<FieldRow label={$t('settings.posting.headers.add_nxg_header')} id="addNgxHeader">
					{#snippet note()}
						<a href="https://github.com/javi11/nxg" class="settings-row-desc link" target="_blank" rel="noreferrer">github.com/javi11/nxg</a>
					{/snippet}
					<input id="addNgxHeader" name="addNgxHeader" type="checkbox" class="toggle toggle-primary" bind:checked={addNgxHeader} />
				</FieldRow>
				<FieldRow
					label={$t('settings.posting.headers.custom_headers.title')}
					description={$t('settings.posting.headers.custom_headers.description')}
				>
					<button type="button" class="btn btn-sm" onclick={addCustomHeader}>
						<Plus class="w-4 h-4" />
						{$t('settings.posting.headers.custom_headers.add_header')}
					</button>
				</FieldRow>
				{#if customHeaders.length > 0}
					<div class="settings-row flex-col items-stretch gap-2 border-t border-(--ui-hairline)">
						{#each customHeaders as _header, index (index)}
							<div class="flex items-center gap-2 max-sm:flex-col max-sm:items-stretch" in:fly={{ y: 6, speed: "fast" }}>
								<input
									class="input input-sm flex-1"
									bind:value={customHeaders[index].name}
									placeholder={$t('settings.posting.headers.custom_headers.header_name_placeholder')}
								/>
								<input
									class="input input-sm flex-1"
									bind:value={customHeaders[index].value}
									placeholder={$t('settings.posting.headers.custom_headers.header_value_placeholder')}
								/>
								<button type="button" class="btn btn-sm btn-ghost text-error" onclick={() => removeCustomHeader(index)}>
									<Trash2 class="w-4 h-4" />
									{$t('settings.posting.headers.custom_headers.remove')}
								</button>
							</div>
						{/each}
					</div>
				{/if}
			</SettingsSection>
		</div>
	{/if}

	<SettingsSection title={$t('settings.posting.newsgroups.title')} description={$t('settings.posting.newsgroups.description')} footer={$t('settings.posting.newsgroups.info')}>
		{#each groups as _group, index (index)}
			<div class="settings-row" in:fly={{ y: 6, speed: "fast" }}>
				<input
					type="checkbox"
					class="toggle toggle-sm toggle-primary"
					aria-label={$t('settings.posting.newsgroups.title')}
					bind:checked={groups[index].enabled}
				/>
				<input
					class="input input-sm flex-1 transition-opacity"
					class:opacity-50={!groups[index].enabled}
					bind:value={groups[index].name}
					placeholder={$t('settings.posting.newsgroups.placeholder')}
					disabled={!groups[index].enabled}
					required
				/>
				{#if groups.length > 1}
					<button type="button" class="btn btn-sm btn-ghost text-error" onclick={() => removeGroup(index)}>
						<Trash2 class="w-4 h-4" />
						{$t('settings.posting.newsgroups.remove')}
					</button>
				{/if}
			</div>
		{/each}
		<div class="settings-row justify-start">
			<button type="button" class="btn btn-sm" onclick={addGroup}>
				<Plus class="w-4 h-4" />
				{$t('settings.posting.newsgroups.add_group')}
			</button>
		</div>
	</SettingsSection>
</div>
