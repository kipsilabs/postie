<script lang="ts">
import FieldRow from "./FieldRow.svelte";

interface Props {
	value?: number;
	id: string;
	label?: string;
	description?: string;
	minValue?: number;
	maxValue?: number;
	unit?: string;
	placeholder?: string;
}

let {
	value = $bindable(0),
	id,
	label = "",
	description = "",
	minValue,
	maxValue,
	unit = "",
	placeholder,
}: Props = $props();
</script>

<FieldRow {label} {description} {id}>
	<div class="join">
		<input
			{id}
			type="number"
			class="input input-sm join-item w-28 text-right tabular-nums"
			bind:value
			min={minValue}
			max={maxValue}
			{placeholder}
		/>
		{#if unit}
			<span class="join-item flex items-center bg-base-200 px-2.5 text-xs text-base-content/60">{unit}</span>
		{/if}
	</div>
</FieldRow>
