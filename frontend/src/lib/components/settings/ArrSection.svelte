<script lang="ts">
	import { t } from '$lib/i18n';
	import apiClient, { type ArrInstance } from '$lib/api/client';
	import { toastStore } from '$lib/stores/toast';
	import FieldRow from '$lib/components/inputs/FieldRow.svelte';
	import ToggleRow from '$lib/components/inputs/ToggleRow.svelte';
	import { fade, fly, listFlip } from '$lib/motion';
	import { collapse } from './collapse';

	let instances = $state<ArrInstance[]>([]);
	let showAddForm = $state(false);
	let saving = $state(false);
	let testingId = $state<string | null>(null);

	function blankInstance(): ArrInstance {
		return {
			id: '',
			name: '',
			type: 'radarr',
			url: '',
			api_key: '',
			enabled: true,
			webhook_id: 0,
			delete_after_upload: false
		};
	}

	let draft = $state<ArrInstance>(blankInstance());

	async function load() {
		try {
			instances = await apiClient.getArrInstances();
		} catch (e) {
			toastStore.error($t('arr.title'), String(e));
		}
	}

	$effect(() => {
		load();
	});

	async function testConnection(instance: ArrInstance) {
		testingId = instance.id || 'draft';
		try {
			await apiClient.testArrConnection(instance);
			toastStore.success($t('arr.connection_ok'));
		} catch (e) {
			toastStore.error($t('arr.error_test'), String(e));
		} finally {
			testingId = null;
		}
	}

	async function setupWebhook() {
		saving = true;
		try {
			const saved = await apiClient.addArrInstance(draft);
			instances = [...instances, saved];
			draft = blankInstance();
			showAddForm = false;
			toastStore.success($t('arr.webhook_created'));
		} catch (e) {
			toastStore.error($t('arr.error_setup'), String(e));
		} finally {
			saving = false;
		}
	}

	async function removeInstance(instance: ArrInstance) {
		try {
			await apiClient.removeArrInstance(instance.id);
			instances = instances.filter((i) => i.id !== instance.id);
			toastStore.success($t('arr.webhook_removed'));
		} catch (e) {
			toastStore.error($t('arr.error_remove'), String(e));
		}
	}

	const ARR_TYPES: { value: ArrInstance['type']; label: string; port: number }[] = [
		{ value: 'radarr', label: 'Radarr', port: 7878 },
		{ value: 'sonarr', label: 'Sonarr', port: 8989 },
		{ value: 'lidarr', label: 'Lidarr', port: 8686 },
		{ value: 'readarr', label: 'Readarr', port: 8787 }
	];

	function applyTypeDefaults() {
		const found = ARR_TYPES.find((t) => t.value === draft.type);
		if (found && !draft.url) {
			draft.url = `http://localhost:${found.port}`;
		}
	}
</script>

<section>
	<div class="flex items-end justify-between gap-3 mb-1.5">
		<div class="section-label !mb-0">{$t('arr.title')}</div>
		{#if !showAddForm}
			<button class="btn btn-sm" onclick={() => (showAddForm = true)} transition:fade={{ speed: "fast" }}>
				+ {$t('arr.add_instance')}
			</button>
		{/if}
	</div>
	<p class="settings-row-desc mx-1 mb-2">{$t('arr.description')}</p>

	<div class="settings-group">
		{#if instances.length === 0 && !showAddForm}
			<div class="settings-row" transition:collapse>
				<p class="text-sm text-base-content/65">{$t('arr.no_instances')}</p>
			</div>
		{/if}

		{#each instances as instance (instance.id)}
			<div class="settings-row" animate:listFlip in:fly={{ y: 6, speed: "fast" }} out:fade={{ speed: "fast" }}>
				<div class="min-w-0 flex-1">
					<span class="settings-row-title">{instance.name}</span>
					<span class="badge badge-ghost badge-sm ml-2">{instance.type}</span>
					<p class="settings-row-desc truncate">{instance.url}</p>
				</div>
				<div class="flex gap-1">
					<button
						class="btn btn-ghost btn-xs"
						disabled={testingId === instance.id}
						onclick={() => testConnection(instance)}
					>
						{testingId === instance.id ? $t('arr.testing') : $t('arr.test_connection')}
					</button>
					<button class="btn btn-ghost btn-xs text-error" onclick={() => removeInstance(instance)}>
						{$t('arr.remove_instance')}
					</button>
				</div>
			</div>
		{/each}

		{#if showAddForm}
			<div transition:collapse class="border-t border-(--ui-hairline)">
				<FieldRow label={$t('arr.instance_name')} id="arr-name">
					<input
						id="arr-name"
						type="text"
						class="input input-sm w-64 max-sm:w-full"
						placeholder={$t('arr.instance_name_placeholder')}
						bind:value={draft.name}
					/>
				</FieldRow>
				<FieldRow label={$t('arr.type')} id="arr-type">
					<select
						id="arr-type"
						class="select select-sm w-auto min-w-40"
						bind:value={draft.type}
						onchange={applyTypeDefaults}
					>
						{#each ARR_TYPES as arrType}
							<option value={arrType.value}>{arrType.label}</option>
						{/each}
					</select>
				</FieldRow>
				<FieldRow label={$t('arr.url')} id="arr-url">
					<input
						id="arr-url"
						type="url"
						class="input input-sm w-64 max-sm:w-full"
						placeholder={$t('arr.url_placeholder')}
						bind:value={draft.url}
					/>
				</FieldRow>
				<FieldRow label={$t('arr.api_key')} id="arr-apikey">
					<input
						id="arr-apikey"
						type="password"
						class="input input-sm w-64 max-sm:w-full"
						placeholder={$t('arr.api_key_placeholder')}
						bind:value={draft.api_key}
					/>
				</FieldRow>
				<ToggleRow id="arr-delete-after-upload" bind:checked={draft.delete_after_upload} label={$t('arr.delete_after_upload')} />
				<div class="settings-row justify-end gap-2">
					<button
						class="btn btn-ghost btn-sm"
						onclick={() => {
							showAddForm = false;
							draft = blankInstance();
						}}
					>
						Cancel
					</button>
					<button
						class="btn btn-ghost btn-sm"
						disabled={testingId === 'draft'}
						onclick={() => testConnection(draft)}
					>
						{testingId === 'draft' ? $t('arr.testing') : $t('arr.test_connection')}
					</button>
					<button
						class="btn btn-primary btn-sm"
						disabled={saving || !draft.name || !draft.url || !draft.api_key}
						onclick={setupWebhook}
					>
						{saving ? $t('arr.setting_up') : $t('arr.save_instance')}
					</button>
				</div>
			</div>
		{/if}
	</div>
</section>
