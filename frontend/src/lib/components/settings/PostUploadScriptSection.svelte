<script lang="ts">
import { t } from "$lib/i18n";
import type { config as configType } from "$lib/wailsjs/go/models";
import FieldRow from "$lib/components/inputs/FieldRow.svelte";
import ToggleRow from "$lib/components/inputs/ToggleRow.svelte";
import SettingsSection from "./SettingsSection.svelte";
import { collapse } from "./collapse";
import DurationInput from "../inputs/DurationInput.svelte";

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

// Initialize config defaults
if (config && !config.post_upload_script) {
	config.post_upload_script = {
		enabled: false,
		command: "",
		timeout: "30s",
		max_retries: 0,
		retry_delay: "5s",
		max_backoff: "5m",
		max_retry_duration: "1h",
		retry_check_interval: "30s",
	};
}

// Reactive local state
let enabled = $state(config.post_upload_script?.enabled ?? false);
let command = $state(config.post_upload_script?.command || "");
let timeout = $state(config.post_upload_script?.timeout || "30s");

// Sync local state back to config
$effect(() => {
	config.post_upload_script.enabled = enabled;
});

$effect(() => {
	config.post_upload_script.command = command;
});

$effect(() => {
	config.post_upload_script.timeout = timeout;
});

</script>

{#if config && config.post_upload_script}
<div class="space-y-6">
	<SettingsSection title={$t('settings.post_upload_script.title')}>
		<ToggleRow
			id="post-upload-script-enabled"
			bind:checked={enabled}
			label={$t('settings.post_upload_script.enable')}
			description={$t('settings.post_upload_script.enable_description')}
		/>

		{#if enabled}
			<div transition:collapse class="border-t border-(--ui-hairline)">
				<FieldRow
					label={$t('settings.post_upload_script.command')}
					description={$t('settings.post_upload_script.command_description')}
					html
					id="script-command"
					stacked
				>
					<input
						id="script-command"
						class="input input-sm w-full font-mono"
						bind:value={command}
						placeholder={$t('settings.post_upload_script.command_placeholder')}
					/>
				</FieldRow>
				<DurationInput
					id="script-timeout"
					label={$t('settings.post_upload_script.timeout')}
					description={$t('settings.post_upload_script.timeout_description')}
					bind:value={timeout}
				/>
			</div>
		{/if}
	</SettingsSection>

	<SettingsSection
		title={$t('settings.post_upload_script.examples.title')}
		description={$t('settings.post_upload_script.examples.description')}
	>
		<div class="overflow-x-auto">
			<table class="table table-xs text-xs font-mono w-full">
				<thead>
					<tr>
						<th class="text-base-content/60">Placeholder</th>
						<th class="text-base-content/60">Resolves to</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td class="font-semibold">{"{nzb_path}"}</td>
						<td class="text-base-content/70">/output/Movies/My Movie.nzb</td>
					</tr>
					<tr>
						<td class="font-semibold">{"{source_path}"}</td>
						<td class="text-base-content/70">/watch/Movies/My Movie/My Movie.mkv</td>
					</tr>
					<tr>
						<td class="font-semibold">{"{source_dir}"}</td>
						<td class="text-base-content/70">/watch/Movies/My Movie</td>
					</tr>
				</tbody>
			</table>
		</div>
		<div class="settings-row flex-col items-stretch gap-3 text-xs font-mono">
			<div>
				<div class="badge badge-ghost badge-sm mb-1">{$t('settings.post_upload_script.examples.webhook')}</div>
				<div class="break-all text-base-content/70">{$t('settings.post_upload_script.examples.webhook_example')}</div>
			</div>
			<div>
				<div class="badge badge-ghost badge-sm mb-1">{$t('settings.post_upload_script.examples.copy_file')}</div>
				<div class="break-all text-base-content/70">{$t('settings.post_upload_script.examples.copy_file_example')}</div>
			</div>
			<div>
				<div class="badge badge-ghost badge-sm mb-1">{$t('settings.post_upload_script.examples.custom_script')}</div>
				<div class="break-all text-base-content/70">{$t('settings.post_upload_script.examples.custom_script_example')}</div>
			</div>
		</div>
	</SettingsSection>
</div>
{:else}
<div class="flex items-center justify-center py-8">
	<p class="text-base-content/65">{$t('settings.post_upload_script.loading')}</p>
</div>
{/if}
