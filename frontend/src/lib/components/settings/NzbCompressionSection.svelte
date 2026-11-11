<script lang="ts">
import { t } from "$lib/i18n";
import type { config as configType } from "$lib/wailsjs/go/models";
import FieldRow from "$lib/components/inputs/FieldRow.svelte";
import NumberField from "$lib/components/inputs/NumberField.svelte";
import ToggleRow from "$lib/components/inputs/ToggleRow.svelte";
import SettingsSection from "./SettingsSection.svelte";
import { collapse } from "./collapse";

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

// Ensure nzb_compression exists with defaults
if (!config.nzb_compression) {
	config.nzb_compression = {
		enabled: false,
		type: "none",
		level: 0,
	};
}

// Reactive local state
let enabled = $state(config.nzb_compression.enabled ?? false);
let compressionType = $state(config.nzb_compression.type || "none");
let compressionLevel = $state(config.nzb_compression.level || 0);

// Dynamic compression types based on translations
let compressionTypes = $derived([
	{
		value: "zstd",
		name: $t("settings.nzb_compression.compression_types.zstd"),
	},
	{
		value: "brotli",
		name: $t("settings.nzb_compression.compression_types.brotli"),
	},
	{
		value: "zip",
		name: $t("settings.nzb_compression.compression_types.zip"),
	},
]);

// Get compression level limits based on type
let footer = $derived.by(() => {
	if (!enabled) return $t('settings.nzb_compression.info.disabled_description');
	if (compressionType === "zstd" || compressionType === "brotli" || compressionType === "zip") {
		return `${$t(`settings.nzb_compression.info.${compressionType}_title`)} ${$t(`settings.nzb_compression.info.${compressionType}_description`)}`;
	}
	return "";
});
let compressionLimits = $derived(getCompressionLimits(compressionType));
let defaultLevel = $derived(getDefaultLevel(compressionType));


// Sync local state back to config
$effect(() => {
	config.nzb_compression.enabled = enabled;
});

$effect(() => {
	config.nzb_compression.type = compressionType;
});

$effect(() => {
	config.nzb_compression.level = compressionLevel;
});

// Auto-adjust level when compression type changes
$effect(() => {
	if (compressionType === "none") {
		compressionLevel = 0;
	} else {
		const limits = getCompressionLimits(compressionType);
		if (compressionLevel < limits.min || compressionLevel > limits.max) {
			compressionLevel = getDefaultLevel(compressionType);
		}
	}
});

function getCompressionLimits(type: string) {
	switch (type) {
		case "zstd":
			return { min: 1, max: 22 };
		case "brotli":
			return { min: 0, max: 11 };
		case "zip":
			return { min: 0, max: 9 };
		default:
			return { min: 0, max: 0 };
	}
}

function getDefaultLevel(type: string) {
	switch (type) {
		case "zstd":
			return 3;
		case "brotli":
			return 4;
		case "zip":
			return 6;
		default:
			return 0;
	}
}


// Auto-set default level when compression type changes
$effect(() => {
  if (
    config.nzb_compression.type !== "none" &&
    config.nzb_compression.level === 0
  ) {
    config.nzb_compression.level = defaultLevel;
  }
});

// Reset level when compression is disabled
$effect(() => {
  if (config.nzb_compression.type === "none") {
    config.nzb_compression.level = 0;
  }
});
</script>

<SettingsSection title={$t('settings.nzb_compression.title')} footer={footer}>
	<ToggleRow
		id="nzb-compression-enabled"
		bind:checked={enabled}
		label={$t('settings.nzb_compression.enable')}
		description={$t('settings.nzb_compression.enable_description')}
	/>

	{#if enabled}
		<div transition:collapse class="border-t border-(--ui-hairline)">
			<FieldRow
				label={$t('settings.nzb_compression.compression_type')}
				description={$t('settings.nzb_compression.compression_type_description')}
				id="compression-type"
			>
				<select id="compression-type" class="select select-sm w-auto min-w-40" bind:value={compressionType}>
					{#each compressionTypes as type}
						<option value={type.value}>{type.name}</option>
					{/each}
				</select>
			</FieldRow>

			{#if compressionType !== "none"}
				<div transition:collapse>
					<NumberField
						id="compression-level"
						bind:value={compressionLevel}
						label={$t('settings.nzb_compression.compression_level')}
						description={$t('settings.nzb_compression.compression_level_description', { values: { min: compressionLimits.min, max: compressionLimits.max } })}
						minValue={compressionLimits.min}
						maxValue={compressionLimits.max}
					/>
				</div>
			{/if}
		</div>
	{/if}
</SettingsSection>
