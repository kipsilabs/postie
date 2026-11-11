<script lang="ts">
import { page } from "$app/state";
import { t } from "$lib/i18n";
import { ArrowLeft, TriangleAlert } from "lucide-svelte";
</script>

<svelte:head>
  <title>{$t("errorPage.document_title", { values: { status: page.status } })}</title>
</svelte:head>

<div class="flex min-h-[50vh] flex-col items-center justify-center gap-3 px-4 text-center animate-fade-up">
  <TriangleAlert class="h-10 w-10 text-base-content/40" strokeWidth={1.5} />

  <div class="space-y-1">
    <h1 class="text-lg font-semibold">
      {page.error?.message || $t("errorPage.title")}
    </h1>
    <p class="text-sm tabular-nums text-base-content/60">{page.status}</p>
    <p class="mx-auto max-w-sm pt-1 text-sm text-base-content/65">
      {$t("errorPage.description")}
    </p>
  </div>

  <div class="mt-2 flex flex-wrap items-center justify-center gap-2">
    <a href="/" class="btn btn-primary btn-sm">
      <ArrowLeft class="h-4 w-4" />
      {$t("errorPage.back")}
    </a>
    <button type="button" class="btn btn-outline btn-sm" onclick={() => window.location.reload()}>
      {$t("errorPage.reload")}
    </button>
  </div>
</div>
