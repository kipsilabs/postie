<script lang="ts">
import FieldRow from "./FieldRow.svelte";
import PresetChips from "./PresetChips.svelte";
// Types
interface ComponentProps {
	value?: number;
	label?: string;
	description?: string;
	placeholder?: string;
	presets?: Array<{ label: string; value: number }>;
	minValue?: number;
	maxValue?: number;
	id?: string;
	unitLabel?: string;
	onchange?: (value: number) => void;
}

// Props
let {
	value = $bindable(0),
	label = "",
	description = "",
	placeholder = "0",
	presets = [],
	minValue = 0,
	maxValue = 1000,
	id = "",
	unitLabel = "MB/s",
	onchange,
}: ComponentProps = $props();

// Effects - trigger callback when value changes
$effect(() => {
	onchange?.(value);
});

// Functions
function setPreset(presetValue: number) {
	value = presetValue;
}
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
		<span class="join-item flex items-center bg-base-200 px-2.5 text-xs text-base-content/60">{unitLabel}</span>
	</div>
	{#snippet below()}
		<PresetChips items={presets.map((p) => ({ label: p.label, onpick: () => setPreset(p.value) }))} />
	{/snippet}
</FieldRow>
