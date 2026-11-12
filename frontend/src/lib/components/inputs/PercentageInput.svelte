<script lang="ts">
import FieldRow from "./FieldRow.svelte";
import PresetChips from "./PresetChips.svelte";
// Types
interface ComponentProps {
	value?: string;
	label?: string;
	description?: string;
	placeholder?: string;
	presets?: Array<{ label: string; value: number }>;
	minValue?: number;
	maxValue?: number;
	id?: string;
}

// Props
let {
	value = $bindable("10%"),
	label = "",
	description = "",
	placeholder = "10",
	presets = [],
	minValue = 1,
	maxValue = 100,
	id = "",
}: ComponentProps = $props();

// State - local state for form input (following best practices)
function parsePercentage(val: string | undefined): number {
	if (!val || typeof val !== "string" || val.trim() === "") {
		return 10;
	}
	return Number.parseInt(val.replace("%", "")) || 10;
}

// Initialize with current value
let percentageValue = $state(parsePercentage(value));
let lastUpdatedValue = $state(value);

// Effects - sync local state back to bound value when user changes input
$effect(() => {
	const newValue = `${percentageValue}%`;
	if (newValue !== lastUpdatedValue) {
		lastUpdatedValue = newValue;
		value = newValue;
	}
});

// Effect to update local state when value prop changes externally (avoid infinite loop)
$effect(() => {
	if (value !== lastUpdatedValue) {
		percentageValue = parsePercentage(value);
		lastUpdatedValue = value;
	}
});

// Functions
function setPreset(presetValue: number) {
	percentageValue = presetValue;
}
</script>

<FieldRow {label} {description} {id}>
	<div class="join">
		<input
			{id}
			type="number"
			class="input input-sm join-item w-28 text-right tabular-nums"
			bind:value={percentageValue}
			min={minValue}
			max={maxValue}
			{placeholder}
		/>
		<span class="join-item flex items-center bg-base-200 px-2.5 text-xs text-base-content/60">%</span>
	</div>
	{#snippet below()}
		<PresetChips items={presets.map((p) => ({ label: p.label, onpick: () => setPreset(p.value) }))} />
	{/snippet}
</FieldRow>
