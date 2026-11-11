<script lang="ts">
import FieldRow from "./FieldRow.svelte";
import PresetChips from "./PresetChips.svelte";
// Imports
import { t } from "$lib/i18n";

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
}

// Props
let {
	value = $bindable(750000),
	label = "",
	description = "",
	placeholder = "750000",
	presets = [],
	minValue = 1000,
	maxValue = 10000000,
	id = "",
}: ComponentProps = $props();

// Derived state
const unitLabel = $derived($t("common.inputs.bytes"));

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
