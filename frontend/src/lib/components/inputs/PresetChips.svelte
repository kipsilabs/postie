<script lang="ts">
interface Props {
	items: Array<{ label: string; onpick: () => void }>;
}

let { items }: Props = $props();
</script>

{#if items.length > 0}
	<div class="flex flex-wrap justify-end gap-1 max-sm:justify-start">
		{#each items as item}
			<button
				type="button"
				class="btn btn-xs btn-ghost rounded-full bg-base-200 font-normal text-base-content/70 hover:bg-base-300 hover:text-base-content"
				onclick={item.onpick}
			>
				{item.label}
			</button>
		{/each}
	</div>
{/if}
