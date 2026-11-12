<script lang="ts">
import { t } from "$lib/i18n";
import { fade, scale } from "$lib/motion";
import { onMount } from "svelte";

interface Props {
	message: string;
	onConfirm: () => void;
	onCancel: () => void;
}

let { message, onConfirm, onCancel }: Props = $props();

let cancelButton = $state<HTMLButtonElement>();

onMount(() => {
	const previous = document.activeElement as HTMLElement | null;
	cancelButton?.focus();
	return () => {
		if (previous?.isConnected) previous.focus();
	};
});

function handleKeydown(event: KeyboardEvent) {
	if (event.key === "Escape") onCancel();
}
</script>

<svelte:window onkeydown={handleKeydown} />

<div
	class="modal modal-open"
	role="dialog"
	aria-modal="true"
	aria-label={$t("common.actions.confirm")}
	out:fade|global={{ speed: "fast" }}
>
	<div class="modal-box max-w-sm" out:scale|global={{ speed: "fast", start: 0.97 }}>
		<h3 class="text-base font-semibold">{$t("common.actions.confirm")}</h3>
		<p class="py-3 text-sm text-base-content/70">{message}</p>
		<div class="modal-action mt-2">
			<button class="btn btn-sm btn-ghost" bind:this={cancelButton} onclick={onCancel}>{$t("common.actions.cancel")}</button>
			<button class="btn btn-sm btn-error" onclick={onConfirm}>{$t("common.actions.delete")}</button>
		</div>
	</div>
	<button aria-label={$t("common.actions.cancel")} class="modal-backdrop" onclick={onCancel}></button>
</div>
