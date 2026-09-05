<script lang="ts">
import apiClient from "$lib/api/client";
import ByteSizeInput from "$lib/components/inputs/ByteSizeInput.svelte";
import PercentageInput from "$lib/components/inputs/PercentageInput.svelte";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import type { config as configType } from "$lib/wailsjs/go/models";
import { Info, ShieldCheck } from "lucide-svelte";

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
let redundancyDisplay = $derived(redundancy || "10%");
</script>

<div class="card bg-base-100 shadow-sm">
  <div class="card-body space-y-6">
    <div class="flex items-center gap-3">
      <ShieldCheck class="w-5 h-5 text-purple-600 dark:text-purple-400" />
      <h2 class="text-lg font-semibold text-base-content">
        {$t('settings.par2.title')}
      </h2>
    </div>

    <div class="space-y-4">
      <div class="flex items-center gap-3">
        <input name="par2enable" type="checkbox" class="checkbox" bind:checked={enabled} />
        <div>
          <label for="par2enable" class="text-base font-medium text-base-content">{$t('settings.par2.enable')}</label>
          <p class="text-sm text-base-content/70">
            {$t('settings.par2.enable_description')}
          </p>
        </div>
      </div>

      {#if enabled}
        <div
          class="ml-6 space-y-6 p-4 bg-base-200 rounded-lg border border-base-300"
        >
          <div class="grid grid-cols-1 md:grid-cols-2 gap-6">
            <div>
              <label for="temp-dir" class="label">
                <span class="label-text">{$t('settings.par2.temp_dir')}</span>
              </label>
              <div class="flex gap-2">
                <input
                  id="temp-dir"
                  class="input input-bordered flex-1"
                  bind:value={tempDir}
                  placeholder={$t('settings.par2.temp_dir_placeholder')}
                />
                {#if apiClient.environment === 'wails'}
                  <button
                    class="btn btn-outline"
                    onclick={selectTempDirectory}
                  >
                    {$t('settings.general.browse')}
                  </button>
                {/if}
              </div>
              <p class="text-sm text-base-content/70 mt-1">
                {$t('settings.par2.temp_dir_description')}
              </p>
            </div>

            <div>
              <label for="parpar-binary-path" class="label">
                <span class="label-text">{$t('settings.par2.parpar_binary_path')}</span>
              </label>
              <input
                id="parpar-binary-path"
                class="input input-bordered w-full"
                bind:value={parparBinaryPath}
                placeholder={$t('settings.par2.parpar_binary_path_placeholder')}
              />
              <p class="text-sm text-base-content/70 mt-1">
                {$t('settings.par2.parpar_binary_path_description')}
              </p>
            </div>

            {#if parparBinaryPath}
            <div class="md:col-span-2">
              <label for="parpar-extra-args" class="label">
                <span class="label-text">{$t('settings.par2.parpar_extra_args')}</span>
              </label>
              <textarea
                id="parpar-extra-args"
                class="textarea textarea-bordered w-full font-mono text-sm"
                rows="3"
                bind:value={parparExtraArgs}
                placeholder={$t('settings.par2.parpar_extra_args_placeholder')}
              ></textarea>
              <p class="text-sm text-base-content/70 mt-1">
                {$t('settings.par2.parpar_extra_args_description')}
              </p>
            </div>
            {/if}

            <!-- Maintain PAR2 Files -->
            <div class="form-control">
              <label for="maintain-par2-files" class="cursor-pointer label">
                <span class="label-text">{$t('settings.par2.maintain_par2_files')}</span>
                <input
                  id="maintain-par2-files"
                  type="checkbox"
                  class="checkbox"
                  bind:checked={maintainPar2Files}
                />
              </label>
              <p class="text-sm text-base-content/70 mt-1">
                {$t('settings.par2.maintain_par2_files_description')}
              </p>
            </div>

            <!-- Skip PAR2 Generation if Files Already Exist -->
            <div class="form-control">
              <label for="skip-if-par2-exists" class="cursor-pointer label">
                <span class="label-text">{$t('settings.par2.skip_if_par2_exists')}</span>
                <input
                  id="skip-if-par2-exists"
                  type="checkbox"
                  class="checkbox"
                  bind:checked={skipIfPar2Exists}
                />
              </label>
              <p class="text-sm text-base-content/70 mt-1">
                {$t('settings.par2.skip_if_par2_exists_description')}
              </p>
            </div>

            <PercentageInput
              bind:value={redundancy}
              label={$t('settings.par2.redundancy')}
              description={$t('settings.par2.redundancy_description')}
              presets={redundancyPresets}
              minValue={1}
              maxValue={50}
              id="redundancy"
            />

            <div>
              <label for="num-goroutines" class="label">
                <span class="label-text">{$t('settings.par2.num_goroutines')}</span>
              </label>
              <input
                id="num-goroutines"
                type="number"
                class="input input-bordered w-full"
                bind:value={numGoroutines}
                min="0"
                max="64"
              />
              <p class="text-sm text-base-content/70 mt-1">
                {$t('settings.par2.num_goroutines_description')}
              </p>
            </div>

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

            <div>
              <label for="par2-max-concurrent-jobs" class="label">
                <span class="label-text">{$t('settings.par2.max_concurrent_jobs')}</span>
              </label>
              <input
                id="par2-max-concurrent-jobs"
                type="number"
                class="input input-bordered w-full"
                bind:value={maxConcurrentJobs}
                min="1"
                max="16"
              />
              <p class="text-sm text-base-content/70 mt-1">
                {$t('settings.par2.max_concurrent_jobs_description')}
              </p>
            </div>

            {#if !parparBinaryPath}
            <div>
              <label for="par2-gf16-method" class="label">
                <span class="label-text">{$t('settings.par2.gf16_method')}</span>
              </label>
              <select
                id="par2-gf16-method"
                class="select select-bordered w-full"
                bind:value={gf16Method}
              >
                {#each gf16Methods as method (method)}
                  <option value={method}>
                    {method === 'auto' ? $t('settings.par2.gf16_method_auto') : method}
                  </option>
                {/each}
              </select>
              <p class="text-sm text-base-content/70 mt-1">
                {$t('settings.par2.gf16_method_description')}
              </p>
            </div>
            {/if}
          </div>

          <div class="space-y-4">
            <div
              class="alert alert-info"
            >
              <div class="flex items-start gap-3">
                <Info
                  class="w-5 h-5 mt-0.5"
                />
                <div>
                  <p
                    class="text-sm font-medium mb-2"
                  >
                    {$t('settings.par2.info.title')}
                  </p>
                  <ul
                    class="text-sm space-y-1 list-disc list-inside"
                  >
                    <li>{$t('settings.par2.info.features.redundancy_percentage_determines_how_much_data_can_be_recovered')}</li>
                    <li>{$t('settings.par2.info.features.higher_redundancy_better_recovery_but_larger_par2_files')}</li>
                  </ul>
                </div>
              </div>
            </div>

            <div class="grid grid-cols-1 gap-4 text-center">
              <div class="p-3 bg-green-50 dark:bg-green-900/20 rounded-lg">
                <div
                  class="text-lg font-semibold text-green-800 dark:text-green-200"
                >
                  {redundancyDisplay}
                </div>
                <div class="text-sm text-green-600 dark:text-green-400">
                  {$t('settings.par2.status.redundancy')}
                </div>
              </div>
            </div>
          </div>
        </div>
      {:else}
        <div
          class="ml-6 p-4 alert alert-warning"
        >
          <p class="text-sm">
            {@html $t('settings.par2.disabled_message')}
          </p>
        </div>
      {/if}
    </div>

  </div>
</div>
