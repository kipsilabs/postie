<script lang="ts">
import { onMount } from "svelte";
import apiClient from "$lib/api/client";
import LanguageSwitcher from "$lib/components/LanguageSwitcher.svelte";
import ThemeSwitcher from "$lib/components/ThemeSwitcher.svelte";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import type { config as configType } from "$lib/wailsjs/go/models";
import { FolderOpen } from "lucide-svelte";
import FieldRow from "$lib/components/inputs/FieldRow.svelte";
import SettingsSection from "./SettingsSection.svelte";
import { collapse } from "./collapse";

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

let isDesktop = $state(false);
let configPath = $state("");
let relocatingConfig = $state(false);

onMount(async () => {
 try {
  await apiClient.initialize();
  isDesktop = apiClient.environment === "wails";
  if (isDesktop) {
   const { GetConfigPath } = await import("$lib/wailsjs/go/backend/App");
   configPath = await GetConfigPath();
  }
 } catch (error) {
  toastStore.error($t("settings.general.config_location_error"), String(error));
 }
});

async function selectConfigDirectory() {
 relocatingConfig = true;
 try {
  const { SelectConfigDirectory } = await import("$lib/wailsjs/go/backend/App");
  const path = await SelectConfigDirectory();
  if (path && path !== configPath) {
   configPath = path;
   toastStore.success($t("settings.general.config_location_success"));
  }
 } catch (error) {
  toastStore.error($t("settings.general.config_location_error"), String(error));
 } finally {
  relocatingConfig = false;
 }
}

// Reactive local state
let outputDir = $state(config.output_dir || "./output");

// Sync local state back to config
$effect(() => {
	config.output_dir = outputDir;
});

async function selectOutputDirectory() {
	try {
		await apiClient.initialize();
		
		if (apiClient.environment !== "wails") {
			toastStore.warning($t("common.messages.wails_only_feature"));
			return;
		}
		
		const { SelectOutputDirectory } = await import("$lib/wailsjs/go/backend/App");
		const dir = await SelectOutputDirectory();
		
		if (dir) {
			outputDir = dir;
		}
	} catch (error) {
		console.error("Failed to select output directory:", error);
		toastStore.error($t("common.messages.error_selecting_directory"), String(error));
	}
}

</script>

<div class="space-y-6">
	<SettingsSection
		title={$t('settings.general.title')}
		footer={`${$t('settings.general.info_title')} ${$t('settings.general.info_description')}`}
	>
		<FieldRow label={$t('settings.general.output_directory')} id="output-dir">
			<div class="flex items-center gap-2 max-sm:flex-col max-sm:items-stretch">
				<input
					id="output-dir"
					class="input input-sm w-72 max-sm:w-full"
					bind:value={outputDir}
					placeholder="./output"
				/>
				{#if apiClient.environment === 'wails'}
					<button type="button" class="btn btn-sm" onclick={selectOutputDirectory}>
						<FolderOpen class="w-4 h-4" />
						{$t('settings.general.browse')}
					</button>
				{/if}
			</div>
		</FieldRow>

		{#if isDesktop}
			<div transition:collapse class="border-t border-(--ui-hairline)">
				<FieldRow
					label={$t('settings.general.config_file')}
					description={$t('settings.general.config_location_description')}
					id="config-path"
				>
					<div class="flex items-center gap-2 max-sm:flex-col max-sm:items-stretch">
						<input id="config-path" class="input input-sm w-72 min-w-0 max-sm:w-full" value={configPath} readonly />
						<button type="button" class="btn btn-sm" onclick={selectConfigDirectory} disabled={relocatingConfig || !configPath}>
							{#if relocatingConfig}<span class="loading loading-spinner loading-xs"></span>{:else}<FolderOpen class="w-4 h-4" />{/if}
							{$t('settings.general.change_config_location')}
						</button>
					</div>
				</FieldRow>
			</div>
		{/if}
	</SettingsSection>

	<SettingsSection
		title={$t('settings.general.ui_preferences')}
		description={$t('settings.general.ui_preferences_description')}
	>
		<FieldRow
			label={$t('settings.general.language')}
			description={$t('settings.general.language_description')}
			id="language-select"
		>
			<LanguageSwitcher />
		</FieldRow>
		<FieldRow
			label={$t('settings.general.theme')}
			description={$t('settings.general.theme_description')}
		>
			<ThemeSwitcher />
		</FieldRow>
	</SettingsSection>
</div>
