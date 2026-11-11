<script lang="ts">
import { onDestroy, onMount } from 'svelte';
import { t } from '$lib/i18n';
import { toastStore } from '$lib/stores/toast';
import apiClient from '$lib/api/client';
import { CircleAlert, Info } from 'lucide-svelte';
import { collapse } from './collapse';

let pendingStatus = $state<Record<string, unknown>>({});
let loading = $state(false);

// Check pending config status on mount and periodically
onMount(async () => {
	await checkPendingStatus();
	
	// Listen for pending config events
	await apiClient.on('config-pending', handlePendingEvent);
	await apiClient.on('config-applied', handleAppliedEvent);
	await apiClient.on('config-updated', handleAppliedEvent);
});

onDestroy(() => {
	apiClient.off('config-pending', handlePendingEvent);
	apiClient.off('config-applied', handleAppliedEvent);
	apiClient.off('config-updated', handleAppliedEvent);
});

async function checkPendingStatus() {
	try {
		pendingStatus = await apiClient.getPendingConfigStatus();
	} catch (error) {
		console.error('Failed to check pending config status:', error);
	}
}

function handlePendingEvent(data: unknown) {
	if (typeof data === 'object' && data !== null) {
		pendingStatus = { ...data as Record<string, unknown> };
	}
}

function handleAppliedEvent(data: unknown) {
	if (typeof data === 'object' && data !== null) {
		pendingStatus = { ...data as Record<string, unknown> };
		toastStore.success(
			$t('settings.pending_config.applied_title'),
			$t('settings.pending_config.applied_message')
		);
	}
}

async function applyNow() {
	if (loading) return;
	
	try {
		loading = true;
		await apiClient.applyPendingConfig();
		
		// Immediately update the pending status after successful apply
		await checkPendingStatus();
		
		toastStore.success(
			$t('settings.pending_config.apply_success'),
			$t('settings.pending_config.apply_success_message')
		);
	} catch (error) {
		console.error('Failed to apply pending config:', error);
		toastStore.error(
			$t('settings.pending_config.apply_error'),
			String(error)
		);
	} finally {
		loading = false;
	}
}

async function discardChanges() {
	if (loading) return;
	
	try {
		loading = true;
		await apiClient.discardPendingConfig();
		
		// Immediately update the pending status after successful discard
		await checkPendingStatus();
		
		toastStore.info(
			$t('settings.pending_config.discard_success'),
			$t('settings.pending_config.discard_message')
		);
	} catch (error) {
		console.error('Failed to discard pending config:', error);
		toastStore.error(
			$t('settings.pending_config.discard_error'),
			String(error)
		);
	} finally {
		loading = false;
	}
}

// Derived state
let hasPending = $derived(Boolean(pendingStatus.hasPendingConfig));
let canApplyNow = $derived(Boolean(pendingStatus.canApplyNow));
let message = $derived(pendingStatus.message as string);
let hasError = $derived(Boolean(pendingStatus.error));
let errorMessage = $derived(pendingStatus.error as string);
</script>

{#if hasPending}
	<div
		transition:collapse
		role={hasError ? "alert" : "status"}
	>
		<div
			class="flex items-start gap-3 rounded-(--radius-box) border px-4 py-3 {hasError
				? 'border-error/40 bg-error/10'
				: 'border-(--ui-hairline) bg-base-100 shadow-sm'}"
		>
			<span class="mt-0.5 shrink-0 {hasError ? 'text-error' : 'text-primary'}">
				{#if hasError}
					<CircleAlert class="w-4 h-4" />
				{:else}
					<Info class="w-4 h-4" />
				{/if}
			</span>

			<div class="min-w-0 flex-1">
				<div class="settings-row-title">
					{#if hasError}
						{$t('settings.pending_config.error_title')}
					{:else}
						{$t('settings.pending_config.pending_title')}
					{/if}
				</div>
				<p class="settings-row-desc">
					{#if hasError}
						{errorMessage}
					{:else}
						<strong>You are viewing and editing unsaved changes.</strong> {message}
					{/if}
				</p>
			</div>

			<div class="flex shrink-0 gap-2 max-sm:flex-col">
				{#if canApplyNow && !hasError}
					<button class="btn btn-sm btn-primary" onclick={applyNow} disabled={loading}>
						{loading ? $t('common.applying') : $t('settings.pending_config.apply_now')}
					</button>
				{/if}
				<button class="btn btn-sm btn-ghost" onclick={discardChanges} disabled={loading}>
					{loading ? $t('common.discarding') : $t('settings.pending_config.discard')}
				</button>
			</div>
		</div>
	</div>
{/if}
