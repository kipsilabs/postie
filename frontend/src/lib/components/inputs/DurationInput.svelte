<script lang="ts">
import FieldRow from "./FieldRow.svelte";
import PresetChips from "./PresetChips.svelte";
// Imports
import { t } from "$lib/i18n";
import { parseDurationInput } from "$lib/duration-input";

// Types
interface ComponentProps {
	value?: string | undefined;
	label?: string;
	description?: string;
	placeholder?: string;
	/** Shown (and saved) when the value is empty: the backend's value for an empty duration. */
	fallback?: string;
	presets?: Array<{ label: string; value: number; unit: string }>;
	minValue?: number;
	maxValue?: number;
	id?: string;
	onchange?: (value: string) => void;
}

// Props
let {
	value = $bindable(),
	label = "",
	description = "",
	placeholder = "5",
	fallback = "5s",
	presets = [],
	minValue = 1,
	maxValue = 3600,
	id = "",
	onchange,
}: ComponentProps = $props();

// State - local state for form inputs (following best practices)
function parseValue(val: string | undefined): { number: number; unit: string } {
	return parseDurationInput(val, fallback);
}

// Initialize with current value
const initial = parseValue(value);
let numberValue = $state(initial.number);
let unitValue = $state(initial.unit);
let lastUpdatedValue = $state(value);

// Effects - sync local state back to bound value when user changes inputs
$effect(() => {
	const newValue = `${numberValue}${unitValue}`;
	if (newValue !== lastUpdatedValue) {
		lastUpdatedValue = newValue;
		value = newValue;
		onchange?.(newValue);
	}
});

// Effect to update local state when value prop changes externally (avoid infinite loop)
$effect(() => {
	if (value !== lastUpdatedValue) {
		const parsed = parseValue(value);
		numberValue = parsed.number;
		unitValue = parsed.unit;
		lastUpdatedValue = value;
	}
});

// Derived state - reactive dropdown options
const timeUnitOptions = $derived([
	{ value: "s", name: $t("common.inputs.time_units.seconds") },
	{ value: "m", name: $t("common.inputs.time_units.minutes") },
	{ value: "h", name: $t("common.inputs.time_units.hours") },
]);

// Functions
function setPreset(presetValue: number, presetUnit: string) {
	numberValue = presetValue;
	unitValue = presetUnit;
}
</script>

<FieldRow {label} {description} {id}>
	<div class="join">
		<input
			{id}
			type="number"
			class="input input-sm join-item w-28 text-right tabular-nums"
			bind:value={numberValue}
			min={minValue}
			max={maxValue}
			{placeholder}
			oninput={(e) => {
				const target = e.target as HTMLInputElement;
				target.value = target.value.replace(/[^0-9]/g, "");
			}}
		/>
		<select class="select select-sm join-item w-auto" aria-label={label || id} bind:value={unitValue}>
			{#each timeUnitOptions as option}
				<option value={option.value}>{option.name}</option>
			{/each}
		</select>
	</div>
	{#snippet below()}
		<PresetChips items={presets.map((p) => ({ label: p.label, onpick: () => setPreset(p.value, p.unit) }))} />
	{/snippet}
</FieldRow>
