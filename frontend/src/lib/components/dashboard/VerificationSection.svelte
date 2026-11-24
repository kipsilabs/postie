<script lang="ts">
import type { backend } from "$lib/api/models";
import { t } from "$lib/i18n";
import { fade, fly, listFlip } from "$lib/motion";
import { verificationActivity, verificationHandoffs } from "$lib/stores/verification";
import { phaseDetail, phaseDot } from "$lib/verification-display";
import { activityRatio as ratio } from "$lib/verification-handoff";
import { ShieldCheck } from "lucide-svelte";
import { onDestroy, onMount } from "svelte";

// items still shown on their upload's progress card are not listed twice
let items = $derived($verificationActivity.filter((a) => !$verificationHandoffs.has(a.id)));
let now = $state(Date.now());
let clock: ReturnType<typeof setInterval> | undefined;

onMount(() => {
	clock = setInterval(() => (now = Date.now()), 1000);
});

onDestroy(() => clearInterval(clock));

function fileName(path: string): string {
	return path.split(/[\\/]/).pop() || path;
}

function detail(item: backend.VerificationActivity): string {
	return phaseDetail(item, $t, now);
}
</script>

{#if items.length > 0}
	<section in:fade>
		<div class="flex items-center justify-between mb-2">
			<div class="section-label !m-0">{$t("dashboard.verification.title")}</div>
			<div class="text-xs text-base-content/60 tabular-nums">
				{$t("dashboard.verification.count", { values: { count: items.length } })}
			</div>
		</div>
		<div class="settings-group">
			{#each items as item (item.id)}
				{@const pct = ratio(item)}
				<div class="settings-row !block" in:fly out:fade animate:listFlip>
					<div class="flex items-center justify-between gap-3">
						<div class="flex items-center gap-2 min-w-0">
							<ShieldCheck class="w-4 h-4 shrink-0 text-base-content/60" />
							<span class="text-sm font-medium truncate" title={item.path}>{fileName(item.path)}</span>
							<span class="inline-flex items-center gap-1.5 text-xs text-base-content/60 shrink-0">
								<span class="size-1.5 rounded-full {phaseDot(item.phase)}"></span>
								{$t(`dashboard.verification.phase.${item.phase}`)}
							</span>
						</div>
						<span class="text-sm tabular-nums shrink-0">{Math.round(pct * 100)}%</span>
					</div>
					<div
						class="progress-track mt-2"
						style="height: 0.25rem"
						role="progressbar"
						aria-valuenow={Math.round(pct * 100)}
						aria-valuemin={0}
						aria-valuemax={100}
						aria-label={fileName(item.path)}
					>
						<div
							class="progress-fill"
							style="transform: scaleX({pct}); {item.phase === 'repairing' ? 'background-color: var(--color-warning)' : ''}"
						></div>
					</div>
					<div class="mt-1.5 text-xs tabular-nums text-base-content/60">{detail(item)}</div>
				</div>
			{/each}
		</div>
	</section>
{/if}
