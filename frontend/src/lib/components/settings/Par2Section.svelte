<script lang="ts">
import apiClient from "$lib/api/client";
import ByteSizeInput from "$lib/components/inputs/ByteSizeInput.svelte";
import PercentageInput from "$lib/components/inputs/PercentageInput.svelte";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import type { config as configType } from "$lib/wailsjs/go/models";
import FieldRow from "$lib/components/inputs/FieldRow.svelte";
import NumberField from "$lib/components/inputs/NumberField.svelte";
import ToggleRow from "$lib/components/inputs/ToggleRow.svelte";
import SettingsSection from "./SettingsSection.svelte";
import { collapse } from "./collapse";

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

// Preset definitions
const redundancyPresets = [
	{ label: "5%", value: 5 },
	{ label: "10%", value: 10 },
	{ label: "15%", value: 15 },
	{ label: "20%", value: 20 },
];

// Reactive local state
let enabled = $state(config.par2?.enabled ?? false);
let tempDir = $state(config.par2?.temp_dir || "");
let redundancy = $state(config.par2?.redundancy || "10%");
let maintainPar2Files = $state(config.par2?.maintain_par2_files ?? false);
let skipIfPar2Exists = $state(config.par2?.skip_if_par2_exists ?? true);
let parparBinaryPath = $state(config.par2?.parpar_binary_path || "");
let parparExtraArgs = $state((config.par2?.parpar_extra_args ?? []).join('\n'));
let numGoroutines = $state(config.par2?.num_goroutines ?? 0);
let memoryLimit = $state(config.par2?.memory_limit ?? 0);
let sliceSize = $state(config.par2?.slice_size ?? 0);
let maxConcurrentJobs = $state(config.par2?.max_concurrent_jobs ?? 1);
let gf16Method = $state(config.par2?.gf16_method || "auto");

const gf16Methods = [
	"auto",
	"lookup",
	"lookup3",
	"shuffle-avx2",
	"shuffle-avx512",
	"shuffle-vbmi",
	"xor-jit-avx2",
	"affine-avx2",
	"affine-avx512",
	"shuffle-neon",
	"clmul-neon",
];

// Sync local state back to config
$effect(() => {
	config.par2.enabled = enabled;
});

$effect(() => {
	config.par2.temp_dir = tempDir;
});

$effect(() => {
	config.par2.redundancy = redundancy;
});

$effect(() => {
	config.par2.maintain_par2_files = maintainPar2Files;
});

$effect(() => {
	config.par2.skip_if_par2_exists = skipIfPar2Exists;
});

$effect(() => {
	config.par2.parpar_binary_path = parparBinaryPath;
});

$effect(() => {
	config.par2.parpar_extra_args = parparExtraArgs.split('\n').map(s => s.trim()).filter(Boolean);
});

$effect(() => {
	config.par2.num_goroutines = numGoroutines;
});

$effect(() => {
	config.par2.memory_limit = memoryLimit;
});

$effect(() => {
	config.par2.slice_size = sliceSize;
});

$effect(() => {
	config.par2.max_concurrent_jobs = maxConcurrentJobs;
});

$effect(() => {
	config.par2.gf16_method = gf16Method;
});

async function selectTempDirectory() {
	try {
		const selectedDir = await apiClient.selectTempDirectory();
		if (selectedDir) {
			tempDir = selectedDir;
		}
	} catch (error) {
		console.error("Failed to select temp directory:", error);
		toastStore.error($t("common.messages.error"), "Failed to select directory");
	}
}

// Display values for status cards
</script>

<SettingsSection
	title={$t('settings.par2.title')}
	footer={enabled
		? `${$t('settings.par2.info.title')} ${$t('settings.par2.info.features.redundancy_percentage_determines_how_much_data_can_be_recovered')}. ${$t('settings.par2.info.features.higher_redundancy_better_recovery_but_larger_par2_files')}.`
		: $t('settings.par2.disabled_message')}
>
	<ToggleRow
		id="par2enable"
		bind:checked={enabled}
		label={$t('settings.par2.enable')}
		description={$t('settings.par2.enable_description')}
	/>

	{#if enabled}
		<div transition:collapse class="border-t border-(--ui-hairline)">
			<FieldRow
				label={$t('settings.par2.temp_dir')}
				description={$t('settings.par2.temp_dir_description')}
				id="temp-dir"
			>
				<div class="flex items-center gap-2 max-sm:flex-col max-sm:items-stretch">
					<input
						id="temp-dir"
						class="input input-sm w-72 max-sm:w-full"
						bind:value={tempDir}
						placeholder={$t('settings.par2.temp_dir_placeholder')}
					/>
					{#if apiClient.environment === 'wails'}
						<button type="button" class="btn btn-sm" onclick={selectTempDirectory}>
							{$t('settings.general.browse')}
						</button>
					{/if}
				</div>
			</FieldRow>

			<FieldRow
				label={$t('settings.par2.parpar_binary_path')}
				description={$t('settings.par2.parpar_binary_path_description')}
				id="parpar-binary-path"
			>
				<input
					id="parpar-binary-path"
					class="input input-sm w-72 max-sm:w-full"
					bind:value={parparBinaryPath}
					placeholder={$t('settings.par2.parpar_binary_path_placeholder')}
				/>
			</FieldRow>

			{#if parparBinaryPath}
				<div transition:collapse>
					<FieldRow
						label={$t('settings.par2.parpar_extra_args')}
						description={$t('settings.par2.parpar_extra_args_description')}
						id="parpar-extra-args"
						stacked
					>
						<textarea
							id="parpar-extra-args"
							class="textarea w-full font-mono text-sm"
							rows="3"
							bind:value={parparExtraArgs}
							placeholder={$t('settings.par2.parpar_extra_args_placeholder')}
						></textarea>
					</FieldRow>
				</div>
			{/if}

			<ToggleRow
				id="maintain-par2-files"
				bind:checked={maintainPar2Files}
				label={$t('settings.par2.maintain_par2_files')}
				description={$t('settings.par2.maintain_par2_files_description')}
			/>
			<ToggleRow
				id="skip-if-par2-exists"
				bind:checked={skipIfPar2Exists}
				label={$t('settings.par2.skip_if_par2_exists')}
				description={$t('settings.par2.skip_if_par2_exists_description')}
			/>

			<PercentageInput
				bind:value={redundancy}
				label={$t('settings.par2.redundancy')}
				description={$t('settings.par2.redundancy_description')}
				presets={redundancyPresets}
				minValue={1}
				maxValue={50}
				id="redundancy"
			/>
			<NumberField
				id="num-goroutines"
				bind:value={numGoroutines}
				label={$t('settings.par2.num_goroutines')}
				description={$t('settings.par2.num_goroutines_description')}
				minValue={0}
				maxValue={64}
			/>
			<ByteSizeInput
				bind:value={memoryLimit}
				label={$t('settings.par2.memory_limit')}
				description={$t('settings.par2.memory_limit_description')}
				minValue={0}
				id="memory-limit"
			/>
			<ByteSizeInput
				bind:value={sliceSize}
				label={$t('settings.par2.slice_size')}
				description={$t('settings.par2.slice_size_description')}
				minValue={0}
				id="slice-size"
			/>
			<NumberField
				id="par2-max-concurrent-jobs"
				bind:value={maxConcurrentJobs}
				label={$t('settings.par2.max_concurrent_jobs')}
				description={$t('settings.par2.max_concurrent_jobs_description')}
				minValue={1}
				maxValue={16}
			/>

			{#if !parparBinaryPath}
				<div transition:collapse>
					<FieldRow
						label={$t('settings.par2.gf16_method')}
						description={$t('settings.par2.gf16_method_description')}
						id="par2-gf16-method"
					>
						<select id="par2-gf16-method" class="select select-sm w-auto min-w-40" bind:value={gf16Method}>
							{#each gf16Methods as method (method)}
								<option value={method}>
									{method === 'auto' ? $t('settings.par2.gf16_method_auto') : method}
								</option>
							{/each}
						</select>
					</FieldRow>
				</div>
			{/if}
		</div>
	{/if}
</SettingsSection>
