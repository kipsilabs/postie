<script lang="ts">
import { duration, easing } from "$lib/motion";
import { Tween } from "svelte/motion";
import { untrack } from "svelte";

interface Props {
	value: number;
	tone?: "warning" | "success" | "error";
}

let { value, tone }: Props = $props();

// Counts tween smoothly; duration is 0 under reduced motion.
const shown = new Tween(untrack(() => value), { duration: () => duration("base"), easing: easing("standard") });

$effect(() => {
	shown.target = value;
});

const toneClass = {
	warning: "text-warning-ink",
	success: "text-success",
	error: "text-error",
} as const;
</script>

<div class="text-xl font-semibold tabular-nums leading-tight {tone && value > 0 ? toneClass[tone] : ''}">
	{Math.round(shown.current).toLocaleString()}
</div>
