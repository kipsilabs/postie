<script lang="ts">
import { AlertTriangle, CheckCircle, Info, X } from "lucide-svelte";
import { onMount } from "svelte";
import { fly, listFlip } from "$lib/motion";
import { t } from "$lib/i18n";
import { type ToastMessage, toastStore } from "../stores/toast";

let toasts: ToastMessage[] = [];

onMount(() => {
	const unsubscribe = toastStore.subscribe((value) => {
		toasts = value;
	});

	return unsubscribe;
});

function getToastColor(type: ToastMessage["type"]) {
	switch (type) {
		case "success":
			return "text-success";
		case "error":
			return "text-error";
		case "warning":
			return "text-warning-ink";
		default:
			return "text-info";
	}
}

function getToastIcon(type: ToastMessage["type"]) {
	switch (type) {
		case "success":
			return CheckCircle;
		case "error":
			return X;
		case "warning":
			return AlertTriangle;
		case "info":
			return Info;
		default:
			return Info;
	}
}

function dismissToast(id: string) {
	toastStore.remove(id);
}
</script>

<!-- Native-style notifications: neutral surface, coloured icon, bottom-right -->
<div class="toast toast-bottom toast-end toast-stack z-[1000]" aria-live="polite" aria-relevant="additions removals">
	{#each toasts as toast (toast.id)}
		{@const Icon = getToastIcon(toast.type)}
		<div
			in:fly={{ x: 24, y: 0, speed: "base", ease: "emphasized" }}
			out:fly={{ x: 24, y: 0, speed: "fast", ease: "in" }}
			animate:listFlip
			class="toast-card"
			role="alert"
			aria-atomic="true"
		>
			<Icon class="mt-0.5 h-5 w-5 flex-none {getToastColor(toast.type)}" />
			<div class="min-w-0 flex-1">
				<div class="font-semibold leading-snug">{toast.title}</div>
				{#if toast.message}
					<div class="text-sm text-base-content/70 break-words">{toast.message}</div>
				{/if}
			</div>
			<button
				class="btn btn-ghost btn-xs btn-square"
				onclick={() => dismissToast(toast.id)}
				aria-label={$t("common.actions.close")}
			>
				<X class="h-4 w-4" />
			</button>
		</div>
	{/each}
</div>

<style>
	.toast-stack {
		padding: 1rem;
		padding-bottom: max(1rem, env(safe-area-inset-bottom));
		max-width: min(26rem, 100vw);
	}
	.toast-card {
		display: flex;
		align-items: flex-start;
		gap: 0.75rem;
		width: 100%;
		padding: 0.75rem 0.75rem 0.75rem 1rem;
		border-radius: var(--radius-box);
		background: var(--color-base-100);
		color: var(--color-base-content);
		box-shadow: var(--ui-shadow-lg);
	}
	@media (max-width: 639px) {
		.toast-stack {
			inset-inline: 0;
			max-width: none;
		}
	}
</style>
