<script lang="ts">
import { t } from "$lib/i18n";
import type { config as configType } from "$lib/wailsjs/go/models";
import ToggleRow from "$lib/components/inputs/ToggleRow.svelte";
import SettingsSection from "./SettingsSection.svelte";

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

// Reactive local state
let maintainOriginalExtension = $state(config.maintain_original_extension ?? true);

// Sync local state back to config
$effect(() => {
	config.maintain_original_extension = maintainOriginalExtension;
});

</script>

<SettingsSection
	title={$t('settings.file_naming.title')}
	footer={`${$t('settings.file_naming.info_title')} ${maintainOriginalExtension ? $t('settings.general.maintain_extension_enabled') : $t('settings.general.maintain_extension_disabled')}`}
>
	<ToggleRow
		id="maintain-original-extension"
		bind:checked={maintainOriginalExtension}
		label={$t('settings.general.maintain_original_extension')}
		description={$t('settings.general.maintain_original_extension_description')}
	/>
</SettingsSection>
