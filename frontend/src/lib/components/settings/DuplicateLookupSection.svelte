<script lang="ts">
import { t } from "$lib/i18n";
import FieldRow from "$lib/components/inputs/FieldRow.svelte";
import ToggleRow from "$lib/components/inputs/ToggleRow.svelte";
import SettingsSection from "./SettingsSection.svelte";
import { collapse } from "./collapse";
import type { config as configType } from "$lib/wailsjs/go/models";
let { config = $bindable() }: { config: configType.ConfigData } = $props();
if (!config.duplicate_lookup) config.duplicate_lookup = { enabled: false, url: "", api_key: "", timeout: "60s" };
let enabled = $state(config.duplicate_lookup.enabled ?? false);
let url = $state(config.duplicate_lookup.url ?? "");
let apiKey = $state(config.duplicate_lookup.api_key ?? "");
let timeout = $state(config.duplicate_lookup.timeout || "60s");
$effect(() => { config.duplicate_lookup.enabled = enabled; config.duplicate_lookup.url = url; config.duplicate_lookup.api_key = apiKey; config.duplicate_lookup.timeout = timeout; });
</script>

<SettingsSection title={$t('settings.duplicate_lookup.title')} description={$t('settings.duplicate_lookup.description')}>
	<ToggleRow id="duplicate-lookup-enabled" bind:checked={enabled} label={$t('settings.duplicate_lookup.enable')} />
	{#if enabled}
		<div transition:collapse class="border-t border-(--ui-hairline)">
			<FieldRow label={$t('settings.duplicate_lookup.url')} id="duplicate-lookup-url" stacked>
				<input id="duplicate-lookup-url" type="url" class="input input-sm w-full" bind:value={url} placeholder="https://lookup.example/api/duplicate-lookup" />
			</FieldRow>
			<div class="border-t border-(--ui-hairline)">
				<FieldRow label={$t('settings.duplicate_lookup.api_key')} id="duplicate-lookup-key">
					<input id="duplicate-lookup-key" type="password" autocomplete="off" class="input input-sm w-64 max-sm:w-full" bind:value={apiKey} />
				</FieldRow>
			</div>
			<div class="border-t border-(--ui-hairline)">
				<FieldRow label={$t('settings.duplicate_lookup.timeout')} id="duplicate-lookup-timeout">
					<input id="duplicate-lookup-timeout" type="text" class="input input-sm w-28 max-sm:w-full" bind:value={timeout} placeholder="60s" />
				</FieldRow>
			</div>
		</div>
	{/if}
</SettingsSection>
