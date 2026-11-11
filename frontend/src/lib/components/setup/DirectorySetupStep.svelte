<script lang="ts">
import apiClient from "$lib/api/client";
import { t } from "$lib/i18n";
import { FolderOpen } from "lucide-svelte";

interface Props {
	outputDirectory?: string;
	watchDirectory?: string;
	onupdate?: (data: {
		outputDirectory: string;
		watchDirectory: string;
	}) => void;
}

let { outputDirectory = "", watchDirectory = "", onupdate }: Props = $props();

function updateDirectories() {
	onupdate?.({ outputDirectory, watchDirectory });
}

async function selectOutputDirectory() {
	try {
		const dir = await apiClient.selectOutputDirectory();
		if (dir) {
			outputDirectory = dir;
			updateDirectories();
		}
	} catch (error) {
		console.error("Failed to select output directory:", error);
	}
}

// Set default output directory if empty
if (!outputDirectory) {
	outputDirectory = "./output";
	updateDirectories();
}
</script>

<div class="mx-auto w-full max-w-xl space-y-5">
	<div class="space-y-1">
		<h2 class="text-lg font-semibold text-base-content">
			{$t("setup.directories.title")}
		</h2>
		<p class="text-sm text-base-content/65">
			{$t("setup.directories.description")}
		</p>
	</div>

	<div class="settings-group">
		<div class="space-y-3 p-4">
			<div>
				<div class="settings-row-title">
					{$t("setup.directories.outputDirectory")}
				</div>
				<div class="settings-row-desc">
					{$t("setup.directories.outputDescription")}
				</div>
			</div>
			<div class="flex flex-col gap-1">
				<label class="text-sm font-medium" for="output-dir">
					{$t("setup.directories.outputPath")} *
				</label>
				<div class="flex gap-2">
					<input
						id="output-dir"
						class="input min-w-0 flex-1"
						bind:value={outputDirectory}
						placeholder="./output"
						required
						oninput={updateDirectories}
					/>
					{#if apiClient.environment === "wails"}
						<button
							class="btn btn-ghost shrink-0"
							onclick={selectOutputDirectory}
						>
							<FolderOpen class="h-4 w-4" />
							{$t("setup.directories.browse")}
						</button>
					{/if}
				</div>
			</div>
		</div>
	</div>

	<p class="text-xs text-base-content/65">* {$t("setup.directories.requiredFields")}</p>
</div>
