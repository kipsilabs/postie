<script lang="ts">
import FieldRow from "./FieldRow.svelte";
import PresetChips from "./PresetChips.svelte";
// Types
interface ComponentProps {
	value?: number;
	label?: string;
	description?: string;
	placeholder?: string;
	presets?: Array<{ label: string; value: number; unit: string }>;
	minValue?: number;
	maxValue?: number;
	id?: string;
	showBytes?: boolean;
	onchange?: (value: number) => void;
}

// Props
let {
	value = $bindable(100),
	label = "",
	description = "",
	placeholder = "100",
	presets = [],
	minValue = 1,
	maxValue = undefined,
	id = "",
	showBytes = false,
	onchange,
}: ComponentProps = $props();

// State - local state for form inputs (following best practices)
function parseSize(val: number | undefined): { value: number; unit: string } {
	if (val === undefined || typeof val !== "number" || Number.isNaN(val)) {
		return { value: 0, unit: "B" };
	}

	// Handle 0 explicitly
	if (val === 0) {
		return { value: 0, unit: "B" };
	}

	if (val >= 1073741824 && val % 1073741824 === 0) {
		return { value: val / 1073741824, unit: "GB" };
	}

	if (val >= 1048576 && val % 1048576 === 0) {
		return { value: val / 1048576, unit: "MB" };
	}

	if (val >= 1024 && val % 1024 === 0) {
		return { value: val / 1024, unit: "KB" };
	}

	return { value: val, unit: "B" };
}

// Initialize with current value
const initial = parseSize(value);
let sizeValue = $state(initial.value);
let sizeUnit = $state(initial.unit);
let lastUpdatedValue = $state(value);

// Effects - sync local state back to bound value when user changes inputs
$effect(() => {
	const newValue = unitToBytes(sizeValue, sizeUnit);
	if (newValue !== lastUpdatedValue) {
		lastUpdatedValue = newValue;
		value = newValue;
		onchange?.(newValue);
	}
});

// Effect to update local state when value prop changes externally (avoid infinite loop)
$effect(() => {
	if (value !== lastUpdatedValue) {
		const parsed = parseSize(value);
		sizeValue = parsed.value;
		sizeUnit = parsed.unit;
		lastUpdatedValue = value;
	}
});

// Derived state - reactive calculations
const sizeUnitOptions = $derived([
	{ value: "B", name: "B" },
	{ value: "KB", name: "KB" },
	{ value: "MB", name: "MB" },
	{ value: "GB", name: "GB" },
]);

const dynamicMaxValue = $derived(
	maxValue
		? sizeUnit === "GB"
			? Math.ceil(maxValue / 1024)
			: maxValue
		: undefined,
);

const byteDisplay = $derived(
	showBytes ? `(${value.toLocaleString()} bytes)` : "",
);

function unitToBytes(val: number, unit: string): number {
	if (unit === "GB") {
		return val * 1024 * 1024 * 1024;
	}
	if (unit === "MB") {
		return val * 1024 * 1024;
	}
	if (unit === "KB") {
		return val * 1024;
	}
	if (unit === "B") {
		return val;
	}
	return val;
}

function setPreset(presetValue: number, presetUnit: string) {
	sizeValue = presetValue;
	sizeUnit = presetUnit;
}
</script>

<FieldRow {label} description={showBytes ? `${description} ${byteDisplay}`.trim() : description} {id}>
	<div class="join">
		<input
			{id}
			type="number"
			class="input input-sm join-item w-28 text-right tabular-nums"
			bind:value={sizeValue}
			min={minValue}
			max={dynamicMaxValue || undefined}
			{placeholder}
		/>
		<select class="select select-sm join-item w-auto" aria-label={label || id} bind:value={sizeUnit}>
			{#each sizeUnitOptions as option}
				<option value={option.value}>{option.name}</option>
			{/each}
		</select>
	</div>
	{#snippet below()}
		<PresetChips items={presets.map((p) => ({ label: `${p.value}${p.unit}`, onpick: () => setPreset(p.value, p.unit) }))} />
	{/snippet}
</FieldRow>
