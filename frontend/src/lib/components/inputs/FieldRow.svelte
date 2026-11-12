<script lang="ts">
import type { Snippet } from "svelte";

interface Props {
	label?: string;
	description?: string;
	/** Render the description as trusted translation HTML. */
	html?: boolean;
	id?: string;
	/** Put the control on its own full-width line below the label (paths, scripts, lists). */
	stacked?: boolean;
	children?: Snippet;
	/** Extra content under the control (preset chips, hints). */
	below?: Snippet;
	/** Extra content under the description. */
	note?: Snippet;
}

let { label = "", description = "", html = false, id = "", stacked = false, children, below, note }: Props = $props();
</script>

<div class="settings-row field-row" class:field-row-stacked={stacked}>
	<div class="field-row-text">
		{#if label}
			{#if id}
				<label class="settings-row-title block" for={id}>{label}</label>
			{:else}
				<div class="settings-row-title">{label}</div>
			{/if}
		{/if}
		{#if description}
			<div class="settings-row-desc">{#if html}{@html description}{:else}{description}{/if}</div>
		{/if}
		{@render note?.()}
	</div>
	<div class="field-row-control">
		{@render children?.()}
		{@render below?.()}
	</div>
</div>

<style>
	.field-row {
		align-items: center;
	}
	.field-row-text {
		min-width: 0;
		flex: 1 1 0;
	}
	.field-row-control {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.375rem;
		flex: 0 1 auto;
		max-width: 100%;
	}
	.field-row-stacked {
		flex-direction: column;
		align-items: stretch;
		gap: 0.5rem;
	}
	.field-row-stacked .field-row-control {
		align-items: stretch;
		width: 100%;
	}
	@media (max-width: 40rem) {
		.field-row {
			flex-direction: column;
			align-items: stretch;
			gap: 0.5rem;
		}
		.field-row-control {
			align-items: stretch;
			width: 100%;
		}
	}
</style>
