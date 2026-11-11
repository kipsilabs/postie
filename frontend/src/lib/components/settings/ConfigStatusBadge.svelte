<script lang="ts">
import { onDestroy, onMount } from 'svelte';
import { t } from '$lib/i18n';
import apiClient from '$lib/api/client';
import { fade } from '$lib/motion';

let hasPending = $state(false);

// Check pending config status on mount and listen for changes
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
		hasPending = await apiClient.hasPendingConfigChanges();
	} catch (error) {
		console.error('Failed to check pending config status:', error);
	}
}

function handlePendingEvent(data: unknown) {
	if (typeof data === 'object' && data !== null) {
		const status = data as Record<string, unknown>;
		hasPending = Boolean(status.hasPendingConfig);
	}
}

function handleAppliedEvent() {
	hasPending = false;
}
</script>

{#if hasPending}
	<div
		class="inline-flex items-center gap-1.5 rounded-full bg-base-200 px-2.5 py-0.5 text-xs text-base-content/70"
		transition:fade={{ speed: "fast" }}
	>
		<span class="h-1.5 w-1.5 rounded-full bg-primary"></span>
		{$t('settings.config_status.editing_pending')}
	</div>
{/if}
