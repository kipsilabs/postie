<script lang="ts">
import { availableLocales, locale, setStoredLocale } from "$lib/i18n";
import { Check, ChevronDown } from "lucide-svelte";

// Get current locale
const languageOptions = availableLocales.map((lang) => ({
	value: lang.code,
	name: `${lang.flag} ${lang.name}`,
}));

async function changeLanguage(event: Event) {
	const target = event.target as HTMLSelectElement;
	const newLocale = target.value;

	// Store the selected locale
	setStoredLocale(newLocale);

	// The sveltekit-i18n library should automatically handle loading the new translations
	// when the locale store is updated.
	locale.set(newLocale);
}
</script>

<select
	id="language-select"
	class="select select-sm w-auto min-w-44 max-sm:w-full"
	value={$locale}
	onchange={changeLanguage}>
	{#each languageOptions as option}
		<option value={option.value}>{option.name}</option>
	{/each}
</select>