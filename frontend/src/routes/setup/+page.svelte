<script lang="ts">
import { goto } from "$app/navigation";
import apiClient from "$lib/api/client";
import SetupWizard from "$lib/components/setup/SetupWizard.svelte";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import type { backend } from "$lib/wailsjs/go/models";

let appStatus = $state<backend.AppStatus | null>(null);
let isLoading = $state(true);

// Check app status on mount
$effect(() => {
	async function checkAppStatus() {
		try {
			const status = await apiClient.getAppStatus();
			appStatus = status;

			// Wizard is only for first-time setup — redirect away on all subsequent visits
			if (!status.isFirstStart) {
				if (status.needsConfiguration) {
					goto("/settings");
				} else {
					goto("/");
				}
				return;
			}

			isLoading = false;
			return;
		} catch (error) {
			console.error("Failed to load app status:", error);
			// Assume setup is needed if we can't get status
			isLoading = false;
		}
	}
	checkAppStatus();
});

// Enhanced error handling for structured API responses
function parseApiError(error: any): { title: string; message: string; details?: string } {
	// Check if it's a structured error response from our API
	if (error?.response?.data?.error) {
		const apiError = error.response.data;
		
		// Map error codes to user-friendly messages
		switch (apiError.code) {
			case 'SERVER_VALIDATION_FAILED':
				return {
					title: $t("setup.errors.server_validation_title"),
					message: $t("setup.errors.server_validation_message"),
					details: apiError.details
				};
			case 'FILESYSTEM_ERROR':
				return {
					title: $t("setup.errors.filesystem_title"),
					message: $t("setup.errors.filesystem_message"),
					details: apiError.details
				};
			case 'CONFIG_SAVE_FAILED':
				return {
					title: $t("setup.errors.config_save_title"),
					message: $t("setup.errors.config_save_message"),
					details: apiError.details
				};
			case 'INVALID_INPUT':
				return {
					title: $t("setup.errors.invalid_input_title"),
					message: $t("setup.errors.invalid_input_message"),
					details: apiError.details
				};
			default:
				return {
					title: $t("setup.errors.generic_title"),
					message: apiError.message || $t("setup.errors.generic_message"),
					details: apiError.details
				};
		}
	}
	
	// Handle network errors
	if (error?.code === 'NETWORK_ERROR' || error?.message?.includes('Network Error')) {
		return {
			title: $t("setup.errors.network_title"),
			message: $t("setup.errors.network_message")
		};
	}
	
	// Handle timeout errors
	if (error?.code === 'ECONNABORTED' || error?.message?.includes('timeout')) {
		return {
			title: $t("setup.errors.timeout_title"),
			message: $t("setup.errors.timeout_message")
		};
	}
	
	// Fallback for unknown errors
	const errorMessage = error instanceof Error ? error.message : String(error);
	return {
		title: $t("common.common.error"),
		message: errorMessage
	};
}

async function handleWizardComplete(
	data: backend.SetupWizardData,
): Promise<void> {
	try {
		await apiClient.setupWizardComplete(data);

		toastStore.success(
			$t("setup.success_title"),
			$t("setup.success_message"),
		);

		// Redirect to dashboard
		goto("/");
		return;
	} catch (error) {
		console.error("Setup wizard failed:", error);
		
		// Parse the error and show user-friendly message
		const { title, message, details } = parseApiError(error);
		
		// Show detailed error in toast
		let fullMessage = message;
		if (details) {
			fullMessage += `\n\n${details}`;
		}
		
		toastStore.error(title, fullMessage);
	}
}

// handleWizardClose was removed since SetupWizard no longer has onclose prop
</script>

<svelte:head>
	<title>{$t("setup.title")} - Postie</title>
	<meta name="description" content="Set up Postie with your NNTP servers and directories" />
</svelte:head>

{#if isLoading}
	<div class="flex items-center justify-center gap-3 p-4 text-sm text-base-content/65" style="min-height: calc(100dvh - var(--titlebar-inset))">
		<span class="loading loading-spinner loading-sm"></span>
		<p>{$t("common.common.loading")}</p>
	</div>
{:else}
	<SetupWizard oncomplete={handleWizardComplete} />
{/if}
