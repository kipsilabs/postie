<script lang="ts">
import { segmentedKeys } from "$lib/actions/segmentedKeys";
import { t } from "$lib/i18n";
import { type Appearance, APPEARANCES, appearance } from "$lib/stores/theme";
import { Monitor, Moon, Sun } from "lucide-svelte";

const icons = { system: Monitor, light: Sun, dark: Moon } as const;
let index = $derived(Math.max(0, APPEARANCES.indexOf($appearance)));

function select(value: Appearance) {
	appearance.set(value);
}
</script>

<div
	class="segmented"
	role="radiogroup"
	use:segmentedKeys
	aria-label={$t("common.appearance.label")}
	style="--n: {APPEARANCES.length}; --i: {index}"
>
	<span class="segmented-thumb" aria-hidden="true"></span>
	{#each APPEARANCES as value (value)}
		{@const Icon = icons[value]}
		<button
			type="button"
			role="radio"
			class="segmented-item"
			aria-checked={$appearance === value}
			tabindex={$appearance === value ? 0 : -1}
			onclick={() => select(value)}
		>
			<Icon class="h-4 w-4" />
			{$t(`common.appearance.${value}`)}
		</button>
	{/each}
</div>
