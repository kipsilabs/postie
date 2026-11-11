<!-- Top strip for screens without the shell (setup, login). On macOS desktop it
     reserves room for the traffic lights and acts as the window drag region. -->
<script lang="ts">
import type { Snippet } from "svelte";

let { children }: { children?: Snippet } = $props();
</script>

<div class="drag-strip">
	{#if children}{@render children()}{/if}
</div>

<style>
	.drag-strip {
		flex: none;
		display: flex;
		align-items: center;
		justify-content: flex-end;
		min-height: var(--titlebar-inset);
		padding-inline: 0.75rem;
		--wails-draggable: drag;
	}
	.drag-strip :global(:is(button, a, input, select)) {
		--wails-draggable: no-drag;
	}
</style>
