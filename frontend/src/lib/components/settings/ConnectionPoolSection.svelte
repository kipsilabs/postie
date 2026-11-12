<script lang="ts">
import DurationInput from "$lib/components/inputs/DurationInput.svelte";
import { t } from "$lib/i18n";
import type { config as configType } from "$lib/wailsjs/go/models";
import NumberField from "$lib/components/inputs/NumberField.svelte";
import SettingsSection from "./SettingsSection.svelte";

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

// Ensure connection_pool exists with defaults
if (!config.connection_pool) {
	config.connection_pool = {
		min_connections: 5,
		health_check_interval: "1m",
	};
}

// Reactive local state
let minConnections = $state(config.connection_pool.min_connections || 5);
let healthCheckInterval = $state(config.connection_pool.health_check_interval || "1m");

// Sync local state back to config
$effect(() => {
	config.connection_pool.min_connections = minConnections;
});

$effect(() => {
	config.connection_pool.health_check_interval = healthCheckInterval;
});


const healthCheckPresets = [
	{ label: "30s", value: 30, unit: "s" },
	{ label: "1m", value: 1, unit: "m" },
	{ label: "5m", value: 5, unit: "m" },
	{ label: "15m", value: 15, unit: "m" },
];
</script>

<SettingsSection
	title={$t('settings.connection_pool.title')}
	footer={`${$t('settings.connection_pool.info_title')} ${$t('settings.connection_pool.info_description')}`}
>
	<NumberField
		id="min-connections"
		bind:value={minConnections}
		label={$t('settings.connection_pool.min_connections')}
		description={$t('settings.connection_pool.min_connections_description')}
		minValue={1}
		maxValue={50}
	/>
	<DurationInput
		bind:value={healthCheckInterval}
		label={$t('settings.connection_pool.health_check_interval')}
		description={$t('settings.connection_pool.health_check_interval_description')}
		presets={healthCheckPresets}
		id="health-check-interval"
	/>
</SettingsSection>
