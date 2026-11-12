<script lang="ts">
import FieldRow from "./FieldRow.svelte";

interface Props {
	checked?: boolean;
	id: string;
	label: string;
	description?: string;
	disabled?: boolean;
}

let { checked = $bindable(false), id, label, description = "", disabled = false }: Props = $props();
</script>

<FieldRow {label} {description} {id}>
	<input {id} type="checkbox" class="toggle toggle-primary" bind:checked {disabled} />
</FieldRow>
