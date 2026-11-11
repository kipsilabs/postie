<script lang="ts">
import logo from "$lib/assets/images/logo.png";
import { t } from "$lib/i18n";
import { fly } from "$lib/motion";
import { backend } from "$lib/wailsjs/go/models";
import DirectorySetupStep from "./DirectorySetupStep.svelte";
import ServerSetupStep from "./ServerSetupStep.svelte";
import WelcomeStep from "./WelcomeStep.svelte";

interface Props {
	oncomplete: (data: backend.SetupWizardData) => void;
}

let { oncomplete }: Props = $props();

let currentStep = $state(1);
// 1 = moving forward (content leaves to the left), -1 = moving back.
let direction = $state<1 | -1>(1);
let hasValidServers = $state(false);

const stepData = new backend.SetupWizardData();

const steps = $derived([
	{ id: 1, name: $t("setup.steps.welcome") },
	{ id: 2, name: $t("setup.steps.servers") },
	{ id: 3, name: $t("setup.steps.directories") },
]);

function nextStep() {
	if (currentStep < steps.length) {
		direction = 1;
		currentStep++;
	}
}

function prevStep() {
	if (currentStep > 1) {
		direction = -1;
		currentStep--;
	}
}

const canProceed = $derived.by(() => {
	switch (currentStep) {
		case 1:
			return true; // Welcome step can always proceed
		case 2:
			// For servers step, require at least one server with successful validation
			return hasValidServers;
		case 3:
			return stepData.outputDirectory !== "";
		default:
			return false;
	}
});

function handleServerUpdate(event: { servers: backend.ServerData[] }) {
	stepData.servers = event.servers;
}

function handleValidationChange(event: { hasValidServers: boolean }) {
	hasValidServers = event.hasValidServers;
}

function handleDirectoryUpdate(event: {
	outputDirectory: string;
}) {
	stepData.outputDirectory = event.outputDirectory;
}

async function finishSetup() {
	oncomplete(stepData);
}

const SLIDE_PX = 28;
</script>

<div class="wizard-wrap">
	<div class="wizard-panel animate-fade-in">
		<!-- Header: logo, title and step indicator -->
		<header class="wizard-header">
			<img src={logo} alt="Postie UI" class="h-9 w-9 shrink-0" loading="lazy" />
			<div class="min-w-0 flex-1">
				<h1 class="truncate text-base font-semibold">
					{$t("setup.wizard_title")}
				</h1>
				<p class="truncate text-sm text-base-content/65">
					{$t("setup.wizard_subtitle")}
				</p>
			</div>
			<div class="flex shrink-0 flex-col items-end gap-1.5" aria-live="polite">
				<div class="flex items-center gap-1.5" aria-hidden="true">
					{#each steps as step (step.id)}
						<span
							class="step-dot"
							class:is-current={currentStep === step.id}
							class:is-done={currentStep > step.id}
						></span>
					{/each}
				</div>
				<span class="text-xs tabular-nums text-base-content/60">
					{$t("setup.step_of", { values: { current: currentStep, total: steps.length } })}
					· {steps[currentStep - 1].name}
				</span>
			</div>
		</header>

		<!-- Step content: old and new steps overlap in one grid cell while sliding -->
		<div class="wizard-body">
			{#key currentStep}
				<div
					class="wizard-step"
					in:fly={{ x: direction * SLIDE_PX, y: 0, speed: "slow", ease: "emphasized" }}
					out:fly={{ x: -direction * SLIDE_PX, y: 0, speed: "base", ease: "in" }}
				>
					{#if currentStep === 1}
						<WelcomeStep />
					{:else if currentStep === 2}
						<ServerSetupStep
							servers={stepData.servers}
							onupdate={handleServerUpdate}
							onvalidationchange={handleValidationChange}
						/>
					{:else if currentStep === 3}
						<DirectorySetupStep
							outputDirectory={stepData.outputDirectory}
							onupdate={handleDirectoryUpdate}
						/>
					{/if}
				</div>
			{/key}
		</div>

		<!-- Navigation: back bottom-left, primary bottom-right -->
		<footer class="wizard-footer">
			<div>
				{#if currentStep > 1}
					<button class="btn btn-ghost btn-sm" onclick={prevStep}>
						{$t("setup.buttons.previous")}
					</button>
				{/if}
			</div>

			{#if currentStep < steps.length}
				<button
					class="btn btn-primary btn-sm"
					disabled={!canProceed}
					onclick={nextStep}
				>
					{$t("setup.buttons.next")}
				</button>
			{:else}
				<button
					class="btn btn-primary btn-sm"
					disabled={!canProceed}
					onclick={finishSetup}
				>
					{$t("setup.buttons.finish")}
				</button>
			{/if}
		</footer>
	</div>
</div>

<style>
	.wizard-wrap {
		display: flex;
		align-items: center;
		justify-content: center;
		min-height: calc(100dvh - var(--titlebar-inset));
		padding: 1rem;
	}
	.wizard-panel {
		display: flex;
		flex-direction: column;
		width: 100%;
		max-width: 48rem;
		min-width: 0;
		border-radius: calc(var(--radius-box) * 1.25);
		background: var(--color-base-100);
		box-shadow: var(--ui-shadow-md);
	}
	.wizard-header {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.875rem 1.25rem;
		border-bottom: 1px solid var(--ui-hairline);
	}
	.wizard-body {
		display: grid;
		min-width: 0;
		min-height: 22rem;
		overflow-x: clip;
	}
	.wizard-step {
		grid-area: 1 / 1;
		min-width: 0;
		padding: 1.5rem 1.25rem;
	}
	.wizard-footer {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0.75rem 1.25rem;
		border-top: 1px solid var(--ui-hairline);
		background: color-mix(in oklab, var(--color-base-content) 3%, transparent);
		border-radius: 0 0 calc(var(--radius-box) * 1.25) calc(var(--radius-box) * 1.25);
	}
	.step-dot {
		height: 0.375rem;
		width: 0.375rem;
		border-radius: 999px;
		background: color-mix(in oklab, var(--color-base-content) 18%, transparent);
		transition:
			width var(--motion-base) var(--ease-standard),
			background-color var(--motion-base) var(--ease-standard);
	}
	.step-dot.is-done {
		background: color-mix(in oklab, var(--color-primary) 45%, transparent);
	}
	.step-dot.is-current {
		width: 1.25rem;
		background: var(--color-primary);
	}
	@media (max-width: 479px) {
		.wizard-header {
			flex-wrap: wrap;
		}
		.wizard-step {
			padding: 1.25rem 1rem;
		}
	}
</style>
