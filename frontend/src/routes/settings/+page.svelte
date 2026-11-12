<script lang="ts">
import { goto } from "$app/navigation";
import apiClient from "$lib/api/client";
import ApiSection from "$lib/components/settings/ApiSection.svelte";
import ConnectionPoolSection from "$lib/components/settings/ConnectionPoolSection.svelte";
import FileNamingSection from "$lib/components/settings/FileNamingSection.svelte";
import GeneralSection from "$lib/components/settings/GeneralSection.svelte";
import DuplicateLookupSection from "$lib/components/settings/DuplicateLookupSection.svelte";
import NzbCompressionSection from "$lib/components/settings/NzbCompressionSection.svelte";
import Par2Section from "$lib/components/settings/Par2Section.svelte";
import PendingConfigNotification from "$lib/components/settings/PendingConfigNotification.svelte";
import ConfigStatusBadge from "$lib/components/settings/ConfigStatusBadge.svelte";
import PostCheckSection from "$lib/components/settings/PostCheckSection.svelte";
import PostingSection from "$lib/components/settings/PostingSection.svelte";
import PostUploadScriptSection from "$lib/components/settings/PostUploadScriptSection.svelte";
import QueueSection from "$lib/components/settings/QueueSection.svelte";
import ServerSection from "$lib/components/settings/ServerSection.svelte";
import WatcherSection from "$lib/components/settings/WatcherSection.svelte";
import ArrSection from "$lib/components/settings/ArrSection.svelte";
import { t } from "$lib/i18n";
import { advancedMode, appStatus, settingsSaveFunction } from "$lib/stores/app";
import { toastStore } from "$lib/stores/toast";
import { backend, config as configType } from "$lib/wailsjs/go/models";
import { AlertCircle, RefreshCw, Save } from "lucide-svelte";
import { collapse } from "$lib/components/settings/collapse";
import { fade, fly } from "$lib/motion";
import { toolbar } from "$lib/stores/toolbar";
import { onDestroy, onMount, tick, untrack } from "svelte";

let configData: configType.ConfigData | null = $state(null);
let localConfig: configType.ConfigData | null = $state(null);
let needsConfiguration = $state(false);
let criticalConfigError = $state(false);
let criticalConfigErrorMessage = $state("");
let loading = $state(false);
let loadError = $state(false);
let saving = $state(false);
// Baseline serialisation of the config; dirty state is derived from data, not DOM events.
let savedConfigSnapshot = $state<string | null>(null);
let isDirty = $derived(
	localConfig !== null &&
		savedConfigSnapshot !== null &&
		JSON.stringify(localConfig) !== savedConfigSnapshot,
);
let activeTab = $state(0);
let formKey = $state(0);

// Sections normalise values on mount (defaults, duration formats). Wait for
// that to settle so a freshly loaded page is clean.
async function settle() {
	await tick();
	await new Promise((resolve) => setTimeout(resolve, 0));
	await tick();
}

async function rebaseline() {
	await settle();
	if (localConfig) savedConfigSnapshot = JSON.stringify(localConfig);
}

// Revealing advanced sections mounts components that normalise values; if the
// page was clean before the toggle it must stay clean.
$effect.pre(() => {
	void $advancedMode;
	untrack(() => {
		if (localConfig && !isDirty) rebaseline();
	});
});

onMount(() => {
	loadConfig();

	// Register save function with the store
	settingsSaveFunction.set(handleSaveConfig);

	// Subscribe to app status
	const unsubscribe = appStatus.subscribe((status: backend.AppStatus) => {
		needsConfiguration = status.needsConfiguration;
		criticalConfigError = status.criticalConfigError;
		criticalConfigErrorMessage = status.error;
	});

	return unsubscribe;
});

async function loadConfig() {
	try {
		loading = true;
		loadError = false;
		await apiClient.initialize();
		const config = await apiClient.getConfig();

		configData = config;
		// Initialize localConfig with the loaded config
		localConfig = JSON.parse(JSON.stringify(config));
		// If no servers exist, add a default one to make it easier for the user
		if (
			localConfig &&
			(!localConfig.servers || localConfig.servers.length === 0)
		) {
			localConfig.servers = [new configType.ServerConfig()];
		}
		savedConfigSnapshot = null;
	} catch (error) {
		console.error("Failed to load config:", error);
		loadError = true;
		configData = null;
		localConfig = null;
	} finally {
		loading = false;
	}

	// After loading=false, components render and run initializeDefaults().
	// tick() waits for that DOM update cycle before capturing the snapshot.
	if (localConfig) {
		await rebaseline();
	}
}

// Reset local edits without unmounting the page (no spinner flash, tab kept).
async function discardChanges() {
	try {
		const config = await apiClient.getConfig();
		configData = config;
		const fresh = JSON.parse(JSON.stringify(config));
		if (!fresh.servers || fresh.servers.length === 0) {
			fresh.servers = [new configType.ServerConfig()];
		}
		localConfig = fresh;
		// Remount sections so their local state re-seeds from the restored config.
		formKey += 1;
		await rebaseline();
	} catch (error) {
		console.error("Failed to discard changes:", error);
		toastStore.error($t("common.common.error"), String(error));
	}
}

async function handleSaveConfig() {
	saving = true;
	try {
		if (!localConfig) {
			toastStore.error(
				$t("common.messages.configuration_error"),
				$t("common.messages.no_configuration_loaded"),
			);
			return;
		}

		// Validate that at least one server is configured
		if (!localConfig.servers || localConfig.servers.length === 0) {
			toastStore.error(
				$t("common.messages.configuration_error"),
				$t("common.messages.no_servers_configured"),
			);
			return;
		}

		// Validate that all servers have required fields
		for (let i = 0; i < localConfig.servers.length; i++) {
			const server = localConfig.servers[i];
			if (!server.host || server.host.trim() === "") {
				toastStore.error(
					$t("common.messages.configuration_error"),
					$t("common.messages.server_host_required", {
						values: { number: i + 1 },
					}),
				);
				return;
			}
			if (!server.port || server.port <= 0 || server.port > 65535) {
				toastStore.error(
					$t("common.messages.configuration_error"),
					$t("common.messages.server_port_required", {
						values: { number: i + 1 },
					}),
				);
				return;
			}
		}

		// Copy the config to a new object to avoid modifying the original
		const configToSave = JSON.parse(JSON.stringify(localConfig));

		// Convert server integer fields
		configToSave.servers = configToSave.servers.map(
			(server: configType.ServerConfig) => ({
				...server,
				port: server.port || 119,
				max_connections: server.max_connections || 10,
				max_connection_idle_time_in_seconds:
					server.max_connection_idle_time_in_seconds || 300,
				max_connection_ttl_in_seconds:
					server.max_connection_ttl_in_seconds || 3600,
			}),
		);

		// Convert posting fields
		configToSave.posting.max_retries =
			Number.parseInt(configToSave.posting.max_retries) || 3;
		configToSave.posting.article_size_in_bytes =
			Number.parseInt(configToSave.posting.article_size_in_bytes) || 750000;

		// Duration fields remain as strings (Go expects human-readable strings like "5s", "1m")

		// Convert queue fields
		if (configToSave.queue) {
			configToSave.queue.max_concurrent_uploads =
				Number.parseInt(configToSave.queue.max_concurrent_uploads) || 1;
		}

		// Convert par2 integer fields
		configToSave.par2.volume_size =
			Number.parseInt(configToSave.par2.volume_size) || 153600000;
		configToSave.par2.max_input_slices =
			Number.parseInt(configToSave.par2.max_input_slices) || 4000;

		// Convert watcher fields
		if (configToSave.watchers && configToSave.watchers.length > 0) {
			configToSave.watchers = configToSave.watchers.map((w: any) => ({
				...w,
				size_threshold: Number.parseInt(w.size_threshold) || 104857600,
				min_file_size: Number.parseInt(w.min_file_size) || 1048576,
			}));
		}

		// Ensure output_dir is set
		if (!configToSave.output_dir || configToSave.output_dir.trim() === "") {
			configToSave.output_dir = "./output";
		}

		await apiClient.saveConfig(configToSave);
		configData = configToSave;
		// Update localConfig to the saved config to maintain reactivity
		localConfig = JSON.parse(JSON.stringify(configToSave));
		savedConfigSnapshot = JSON.stringify(localConfig);

		// Update app status
		const status = await apiClient.getAppStatus();
		appStatus.set(status);

		toastStore.success(
			$t("common.messages.configuration_saved"),
			$t("common.messages.configuration_saved_description"),
		);

		// Redirect to dashboard if configuration was needed
		if (needsConfiguration) {
			goto("/");
		}
	} catch (error) {
		console.error("Failed to save config:", error);
		toastStore.error(
			$t("common.messages.configuration_save_failed"),
			String(error),
		);
	} finally {
		saving = false;
	}
}

function handleBeforeUnload(event: BeforeUnloadEvent) {
	if (isDirty) {
		event.preventDefault();
	}
}

$effect(() => {
	toolbar.set({ actions: toolbarActions });
	return () => toolbar.reset();
});

onDestroy(() => {
	// Clean up when the component is destroyed
	settingsSaveFunction.set(null);
});
</script>

{#snippet toolbarActions()}
  <ConfigStatusBadge />
  <label class="flex cursor-pointer items-center gap-2" title={$t('settings.header.advanced_mode_description')}>
    <span class="text-sm text-base-content/70 max-sm:hidden">{$t('settings.header.advanced_mode')}</span>
    <input type="checkbox" class="toggle toggle-primary toggle-sm" aria-label={$t('settings.header.advanced_mode')} bind:checked={$advancedMode} />
  </label>
{/snippet}

<svelte:window onbeforeunload={handleBeforeUnload} />

<svelte:head>
  <title>{isDirty ? '● ' : ''}{$t('settings.title')} - Postie</title>
  <meta name="description" content="Configure your upload settings" />
</svelte:head>

<div class="space-y-5">
  <p class="settings-row-desc mx-1">{$t('settings.header.description')}</p>

  {#if criticalConfigError}
    <div transition:collapse role="alert">
      <div class="flex items-start gap-3 rounded-(--radius-box) border border-error/40 bg-error/10 px-4 py-3">
        <AlertCircle class="mt-0.5 h-4 w-4 shrink-0 text-error" />
        <p class="text-sm">
          <strong>{$t('settings.header.status.configuration_error')}:</strong>
          {criticalConfigErrorMessage}
        </p>
      </div>
    </div>
  {:else if needsConfiguration}
    <div transition:collapse role="status">
      <div class="flex items-start gap-3 rounded-(--radius-box) border border-warning/40 bg-warning/10 px-4 py-3">
        <AlertCircle class="mt-0.5 h-4 w-4 shrink-0 text-warning-ink" />
        <p class="text-sm">
          <strong>{$t('settings.header.alerts.setup_required')}</strong> {$t('settings.header.alerts.setup_required_description')}
        </p>
      </div>
    </div>
  {/if}

  {#if loading === true}
    <div class="flex items-center justify-center py-12">
      <div class="text-center">
        <div class="loading loading-spinner w-8 h-8 mb-4 mx-auto"></div>
        <p class="text-base-content/70">{$t('common.common.loading')}</p>
      </div>
    </div>
  {:else if loadError}
    <div class="flex items-center justify-center py-12" in:fade={{ speed: "base" }}>
      <div class="text-center max-w-md">
        <AlertCircle class="mb-4 w-12 h-12 mx-auto text-error" />
        <h3 class="mb-2 text-lg font-semibold">
          {$t('settings.header.status.failed_to_load_configuration')}
        </h3>
        <p class="mb-4 text-base-content/70">
          {$t('settings.header.status.failed_to_load_configuration_description')}
        </p>
        <button class="btn btn-primary btn-sm" onclick={loadConfig}>
          <RefreshCw class="w-4 h-4 mr-2" />
          {$t('settings.retry')}
        </button>
      </div>
    </div>
  {:else if localConfig}
    <!-- Pending Config Notification -->
    <PendingConfigNotification />

    {#key formKey}
    <div>
      <div role="tablist" class="tabs settings-tabs">
        <input type="radio" name="settings_tabs" role="tab" class="tab" aria-label="Core Configuration" checked={activeTab === 0} onchange={() => (activeTab = 0)} />
        <div role="tabpanel" class="tab-content">
          <div class="space-y-6">
            <GeneralSection bind:config={localConfig} />
            <ServerSection bind:config={localConfig} />
          </div>
        </div>

        <input type="radio" name="settings_tabs" role="tab" class="tab" aria-label="Upload Settings" checked={activeTab === 1} onchange={() => (activeTab = 1)} />
        <div role="tabpanel" class="tab-content">
          <div class="space-y-6">
            <PostingSection bind:config={localConfig} />
            <PostCheckSection bind:config={localConfig} />
            {#if $advancedMode}
              <div transition:collapse class="space-y-6">
                <QueueSection bind:config={localConfig} />
                <ConnectionPoolSection bind:config={localConfig} />
              </div>
            {/if}
          </div>
        </div>

        <input type="radio" name="settings_tabs" role="tab" class="tab" aria-label="File Processing" checked={activeTab === 2} onchange={() => (activeTab = 2)} />
        <div role="tabpanel" class="tab-content">
          <div class="space-y-6">
            <FileNamingSection bind:config={localConfig} />
            <Par2Section bind:config={localConfig} />
            <DuplicateLookupSection bind:config={localConfig} />
            <NzbCompressionSection bind:config={localConfig} />
          </div>
        </div>

        <input type="radio" name="settings_tabs" role="tab" class="tab" aria-label="Automation" checked={activeTab === 3} onchange={() => (activeTab = 3)} />
        <div role="tabpanel" class="tab-content">
          <div class="space-y-6">
            <WatcherSection bind:config={localConfig} />
            <PostUploadScriptSection bind:config={localConfig} />
            <ApiSection bind:config={localConfig} />
            {#if apiClient.environment !== 'wails'}
              <ArrSection />
            {/if}
          </div>
        </div>
      </div>
    </div>
    {/key}

    <!-- Save bar: slides in only when there is something to save -->
    {#if isDirty || needsConfiguration}
      <div class="sticky bottom-0 z-10 -mx-1 pb-1 pt-2" transition:fly={{ y: 16 }}>
        <div class="settings-group">
          <div class="settings-row">
            <span class="text-sm {isDirty ? 'text-base-content' : 'text-base-content/60'}">
              {#if isDirty}
                <span class="mr-1.5 inline-block h-1.5 w-1.5 rounded-full bg-primary align-middle"></span>
                {$t('settings.header.unsaved_changes')}
              {/if}
            </span>
            <div class="flex items-center gap-2">
              {#if isDirty}
                <button type="button" class="btn btn-sm btn-ghost" onclick={discardChanges} disabled={saving} transition:fade={{ speed: "fast" }}>
                  {$t('settings.header.discard_changes')}
                </button>
              {/if}
              <button type="button" class="btn btn-sm btn-primary" onclick={handleSaveConfig} disabled={saving}>
                {#if saving}
                  <span class="loading loading-spinner loading-sm"></span>
                  {$t('common.common.saving')}
                {:else}
                  <Save class="w-4 h-4" />
                  {$t('settings.header.save_configuration')}
                {/if}
              </button>
            </div>
          </div>
        </div>
      </div>
    {/if}
  {:else}
    <div class="flex items-center justify-center py-12">
      <div class="text-center">
        <p class="text-base-content/60">{$t('settings.no_configuration_available')}</p>
      </div>
    </div>
  {/if}
</div>

<style>
  /* Segmented-style secondary navigation built on daisyUI radio tabs. The radios
     stay real `input[role=tab]` elements (browser tests click them). */
  .settings-tabs {
    gap: 2px;
    row-gap: 0.25rem;
  }
  .settings-tabs > :global(input.tab) {
    flex: 0 0 auto;
    height: calc(var(--size-field, 0.25rem) * 8);
    min-width: 0;
    padding-inline: 0.875rem;
    border: 0;
    border-radius: var(--radius-field);
    font-size: 0.875rem;
    font-weight: 500;
    color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
    background: transparent;
    margin-bottom: 0.75rem;
  }
  .settings-tabs > :global(input.tab:hover) {
    background: color-mix(in oklab, var(--color-base-content) 6%, transparent);
    color: var(--color-base-content);
  }
  .settings-tabs > :global(input.tab:checked) {
    background: color-mix(in oklab, var(--color-base-content) 10%, transparent);
    color: var(--color-base-content);
  }
  .settings-tabs > :global(input.tab:focus-visible) {
    outline: var(--focus-width, 2px) solid var(--focus-color, var(--color-primary));
    outline-offset: var(--focus-offset, 2px);
  }
  .settings-tabs > :global(.tab-content) {
    padding: 0;
    border: 0;
    background: transparent;
  }
  .settings-tabs > :global(input.tab:checked + .tab-content) {
    /* backwards (not both): a retained end transform would make fixed modals inside the panel position against it */
    animation: fade-up var(--motion-base) var(--ease-standard) backwards;
  }
  @media (max-width: 40rem) {
    .settings-tabs > :global(input.tab) {
      padding-inline: 0.625rem;
      font-size: 0.8125rem;
    }
  }
</style>
