<script lang="ts">
import type { Snippet } from "svelte";

interface Props {
	title: string;
	description?: string;
	/** Muted footnote under the group (replaces decorative info alerts). */
	footer?: string;
	children?: Snippet;
}

let { title, description = "", footer = "", children }: Props = $props();
</script>

<section class="settings-section">
	<div class="section-label">{title}</div>
	{#if description}
		<p class="settings-row-desc mx-1 mb-2">{description}</p>
	{/if}
	<div class="settings-group">
		{@render children?.()}
	</div>
	{#if footer}
		<p class="settings-row-desc mx-1 mt-1.5">{@html footer}</p>
	{/if}
</section>
