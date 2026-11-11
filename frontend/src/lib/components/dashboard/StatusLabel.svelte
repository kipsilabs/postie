<script lang="ts">
import { getStatusIconClass } from "$lib/utils";

let { status }: { status: string } = $props();
</script>

<span class="inline-flex items-center gap-1.5 text-xs {getStatusIconClass(status)}">
	<span class="size-1.5 rounded-full bg-current"></span>
	<span class="text-base-content/80 capitalize">{status}</span>
</span>
