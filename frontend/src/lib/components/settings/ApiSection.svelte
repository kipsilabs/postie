<script lang="ts">
import apiClient from "$lib/api/client";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import type { config as configType } from "$lib/wailsjs/go/models";
import { Copy, Eye, EyeOff, RefreshCw } from "lucide-svelte";
import FieldRow from "$lib/components/inputs/FieldRow.svelte";
import ToggleRow from "$lib/components/inputs/ToggleRow.svelte";
import { fade, scale } from "$lib/motion";
import SettingsSection from "./SettingsSection.svelte";
import { collapse } from "./collapse";
import { onMount } from "svelte";

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

let enabled = $state(config.api?.enabled ?? false);
let apiKey = $state("");
let revealed = $state(false);
let loading = $state(false);
let regenerating = $state(false);
let confirmRegenerate = $state(false);

const displayedKey = $derived(
	apiKey ? (revealed ? apiKey : "•".repeat(Math.min(apiKey.length, 32))) : ""
);

const curlExample = $derived(
	apiKey
		? `curl -X POST -H "X-API-Key: ${apiKey}" \\\n  -H "Content-Type: application/json" \\\n  -d '{"file":"/mnt/local/Media/Movie/Movie 1/movie.mkv","relative_path":"/mnt/local","delete_after_upload":false}' \\\n  http://localhost:8080/api/v1/queue/upload`
		: ""
);

$effect(() => {
	if (!config.api) {
		config.api = { enabled: false };
	}
	config.api.enabled = enabled;
});

onMount(async () => {
	if (!enabled) return;
	await loadKey();
});

async function loadKey() {
	loading = true;
	try {
		apiKey = await apiClient.getApiKey();
	} catch (error) {
		console.error("Failed to load API key:", error);
		toastStore.error($t("settings.api.load_failed"), String(error));
	} finally {
		loading = false;
	}
}

async function regenerate() {
	regenerating = true;
	try {
		apiKey = await apiClient.regenerateApiKey();
		toastStore.success(
			$t("settings.api.regenerated_success"),
			$t("settings.api.regenerated_success_description")
		);
	} catch (error) {
		console.error("Failed to regenerate API key:", error);
		toastStore.error($t("settings.api.regenerate_failed"), String(error));
	} finally {
		regenerating = false;
		confirmRegenerate = false;
	}
}

async function copyKey() {
	if (!apiKey) return;
	try {
		await navigator.clipboard.writeText(apiKey);
		toastStore.success($t("settings.api.copied"), "");
	} catch (error) {
		console.error("Failed to copy:", error);
	}
}

$effect(() => {
	if (enabled && !apiKey && !loading) {
		void loadKey();
	}
});
</script>

<SettingsSection title={$t("settings.api.title")}>
	<ToggleRow
		id="api-enabled"
		bind:checked={enabled}
		label={$t("settings.api.enabled")}
		description={$t("settings.api.enabled_description")}
	/>

	{#if enabled}
		<div transition:collapse class="border-t border-(--ui-hairline)">
			<FieldRow
				label={$t("settings.api.key_label")}
				description={$t("settings.api.key_description")}
				id="api-key"
				stacked
			>
				<div class="join w-full">
					<input
						id="api-key"
						type="text"
						readonly
						class="input input-sm join-item flex-1 font-mono"
						value={displayedKey}
						placeholder={loading ? "…" : ""}
					/>
					<button
						type="button"
						class="btn btn-sm join-item"
						onclick={() => (revealed = !revealed)}
						title={revealed ? $t("settings.api.hide") : $t("settings.api.reveal")}
						aria-label={revealed ? $t("settings.api.hide") : $t("settings.api.reveal")}
						disabled={!apiKey}
					>
						{#if revealed}
							<EyeOff class="w-4 h-4" />
						{:else}
							<Eye class="w-4 h-4" />
						{/if}
					</button>
					<button
						type="button"
						class="btn btn-sm join-item"
						onclick={copyKey}
						title={$t("settings.api.copy")}
						aria-label={$t("settings.api.copy")}
						disabled={!apiKey}
					>
						<Copy class="w-4 h-4" />
					</button>
					<button
						type="button"
						class="btn btn-sm join-item"
						onclick={() => (confirmRegenerate = true)}
						disabled={regenerating}
						title={$t("settings.api.regenerate")}
						aria-label={$t("settings.api.regenerate")}
					>
						<RefreshCw class="w-4 h-4 {regenerating ? 'animate-spin' : ''}" />
					</button>
				</div>
			</FieldRow>

			{#if apiKey}
				<div transition:collapse>
					<FieldRow label={$t("settings.api.curl_example_label")} id="api-curl" stacked>
						<textarea
							id="api-curl"
							readonly
							class="textarea w-full font-mono text-xs h-28"
							value={curlExample}
						></textarea>
					</FieldRow>
				</div>
			{/if}
		</div>
	{/if}
</SettingsSection>

{#if confirmRegenerate}
	<div class="modal modal-open" role="dialog" aria-modal="true" out:fade={{ speed: "fast" }}>
		<div class="modal-box max-w-md" out:scale={{ speed: "fast", start: 0.97 }}>
			<h3 class="text-base font-semibold mb-2">{$t("settings.api.regenerate")}</h3>
			<p class="text-sm text-base-content/70">{$t("settings.api.regenerate_confirm")}</p>
			<div class="modal-action">
				<button
					type="button"
					class="btn btn-sm btn-ghost"
					onclick={() => (confirmRegenerate = false)}
					disabled={regenerating}
				>
					{$t("common.common.cancel")}
				</button>
				<button type="button" class="btn btn-sm btn-primary" onclick={regenerate} disabled={regenerating}>
					<RefreshCw class="w-4 h-4 {regenerating ? 'animate-spin' : ''}" />
					{regenerating ? $t("common.common.saving") : $t("settings.api.regenerate")}
				</button>
			</div>
		</div>
	</div>
{/if}
