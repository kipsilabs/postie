<!-- Setup layout: no sidebar. The root layout provides styles and toasts. -->
<script lang="ts">
import apiClient from "$lib/api/client";
import type { Snippet } from "svelte";
import { onMount } from "svelte";

let { children }: { children: Snippet } = $props();

onMount(async () => {
	// Initialize API client
	await apiClient.initialize();
});
</script>

<div class="min-h-screen bg-base-200">
	{@render children()}
</div>
