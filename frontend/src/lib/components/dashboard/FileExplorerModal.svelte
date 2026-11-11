<script lang="ts">
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import apiClient from "$lib/api/client";
import { fade, scale } from "$lib/motion";
import { X, Upload, Loader, ArrowLeft, Home, Folder, File as FileIcon } from "lucide-svelte";
import { onMount } from "svelte";

interface FileSystemItem {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  modTime: string;
}

interface FileManagerItem {
  id: string;
  size: number;
  date: Date;
  type: "folder" | "file";
}

interface Props {
  isOpen: boolean;
  onClose: () => void;
}

let { isOpen, onClose }: Props = $props();

let fileManagerData = $state<FileManagerItem[]>([]);
let selectedFiles = $state<Set<string>>(new Set());
let importing = $state(false);
let loading = $state(false);
let pathInput = $state("");
let pathHistory = $state<string[]>([]);

let selectedFileCount = $derived(selectedFiles.size);
let hasSelectedFiles = $derived(selectedFiles.size > 0);

let currentPath = $state("/");

async function loadDirectory(path: string = "/"): Promise<FileManagerItem[]> {
  try {
    const response = await apiClient.browseFilesystem(path);
    // Convert our API response to SVAR FileManager format
    const items: FileManagerItem[] = response.items.map((item: FileSystemItem) => ({
      id: item.path,
      size: item.size,
      date: new Date(item.modTime),
      type: item.isDir ? "folder" : "file"
    }));

    return items;
  } catch (error) {
    console.error("Failed to load directory:", error);
    toastStore.error($t("common.messages.error"), String(error));
    return [];
  }
}

async function navigateToPath(path: string) {
  if (path !== currentPath) {
    pathHistory.push(currentPath);
  }
  
  loading = true;
  try {
    const items = await loadDirectory(path);
    fileManagerData = items;
    currentPath = path;
    pathInput = path;
    
    // Clear selection when navigating
    selectedFiles.clear();
    selectedFiles = new Set(selectedFiles);
  } finally {
    loading = false;
  }
}

async function loadInitialData() {
  loading = true;
  try {
    fileManagerData = await loadDirectory("/");
    pathInput = "/";
  } finally {
    loading = false;
  }
}

function goBack() {
  if (pathHistory.length > 0) {
    const previousPath = pathHistory.pop()!;
    navigateToPath(previousPath);
  }
}

function goHome() {
  pathHistory = [];
  navigateToPath("/");
}

function handlePathInputKeydown(event: KeyboardEvent) {
  if (event.key === "Enter") {
    navigateToPath(pathInput);
  }
}

async function importSelectedFiles() {
  if (selectedFiles.size === 0) {
    toastStore.error($t("common.messages.error"), $t("dashboard.file_explorer.no_files_selected"));
    return;
  }

  importing = true;
  try {
    const filePaths = Array.from(selectedFiles);
    await apiClient.importFiles(filePaths);

    toastStore.success(
      $t("dashboard.file_explorer.import_success"),
      $t("dashboard.file_explorer.import_success_description", { values: { count: filePaths.length } })
    );
    
    // Clear selection and close modal
    selectedFiles.clear();
    selectedFiles = new Set(selectedFiles);
    onClose();
  } catch (error) {
    console.error("Failed to import files:", error);
    toastStore.error($t("common.messages.error"), String(error));
  } finally {
    importing = false;
  }
}

function toggleSelected(id: string) {
  if (selectedFiles.has(id)) {
    selectedFiles.delete(id);
  } else {
    selectedFiles.add(id);
  }
  selectedFiles = new Set(selectedFiles);
}

function selectAll() {
  for (const item of fileManagerData) {
    selectedFiles.add(item.id);
  }
  selectedFiles = new Set(selectedFiles);
}

function clearSelection() {
  selectedFiles.clear();
  selectedFiles = new Set(selectedFiles);
}

function handleKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    onClose();
  }
}

function handleBackdropKeydown(event: KeyboardEvent) {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    onClose();
  }
}

let modalRef: HTMLDivElement | undefined = $state();
let previouslyFocused: HTMLElement | null = null;

// Load initial data when modal opens
$effect(() => {
  if (isOpen) {
    loadInitialData();
    // Store previously focused element and focus the modal
    previouslyFocused = document.activeElement as HTMLElement;
    // Use tick to wait for DOM
    requestAnimationFrame(() => {
      const firstFocusable = modalRef?.querySelector<HTMLElement>(
        'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
      );
      firstFocusable?.focus();
    });
  } else if (previouslyFocused) {
    // Restore focus when modal closes
    previouslyFocused.focus();
    previouslyFocused = null;
  }
});

function handleFocusTrap(event: KeyboardEvent) {
  if (event.key !== "Tab" || !modalRef) return;

  const focusableElements = modalRef.querySelectorAll<HTMLElement>(
    'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
  );
  if (focusableElements.length === 0) return;

  const first = focusableElements[0];
  const last = focusableElements[focusableElements.length - 1];

  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
}

onMount(() => {
  document.addEventListener("keydown", handleKeydown);
  return () => {
    document.removeEventListener("keydown", handleKeydown);
  };
});
</script>

{#if isOpen}
  <!-- Modal backdrop -->
  <div
    class="fixed inset-0 z-50 bg-black/40"
    style="-webkit-backdrop-filter: blur(var(--modal-blur)); backdrop-filter: blur(var(--modal-blur))"
    onclick={onClose}
    onkeydown={handleBackdropKeydown}
    role="presentation"
    tabindex="-1"
    in:fade
    out:fade={{ speed: "fast" }}
  ></div>

  <!-- Modal content -->
  <div
    class="fixed inset-0 z-50 flex items-center justify-center p-4 pointer-events-none"
    role="dialog"
    aria-modal="true"
    aria-labelledby="file-explorer-title"
  >
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      bind:this={modalRef}
      class="bg-base-100 rounded-[var(--radius-box)] shadow-lg w-full max-w-4xl h-[80vh] flex flex-col pointer-events-auto overflow-hidden"
      onkeydown={handleFocusTrap}
      in:scale
      out:scale={{ speed: "fast", start: 0.97 }}
    >
      <!-- Header -->
      <div class="flex items-center justify-between px-4 py-3 border-b border-[var(--ui-hairline)]">
        <h2 id="file-explorer-title" class="text-base font-semibold text-base-content">
          {$t("dashboard.file_explorer.title")}
        </h2>
        <button class="btn btn-ghost btn-square btn-sm" onclick={onClose} aria-label={$t("dashboard.file_explorer.close")} title={$t("dashboard.file_explorer.close")}>
          <X class="w-4 h-4" />
        </button>
      </div>

      <!-- Navigation toolbar -->
      <div class="px-3 py-2 bg-base-200/60 border-b border-[var(--ui-hairline)] space-y-2">
        <div class="flex items-center gap-1.5">
          <button
            class="btn btn-ghost btn-square btn-sm"
            onclick={goBack}
            disabled={pathHistory.length === 0}
            title={$t('dashboard.file_explorer.go_back_tooltip')}
            aria-label={$t('dashboard.file_explorer.go_back_tooltip')}
          >
            <ArrowLeft class="w-4 h-4" />
          </button>

          <button
            class="btn btn-ghost btn-square btn-sm"
            onclick={goHome}
            title={$t('dashboard.file_explorer.go_home_tooltip')}
            aria-label={$t('dashboard.file_explorer.go_home_tooltip')}
          >
            <Home class="w-4 h-4" />
          </button>

          <div class="flex-1">
            <input
              type="text"
              class="input input-sm w-full"
              bind:value={pathInput}
              onkeydown={handlePathInputKeydown}
              placeholder={$t('dashboard.file_explorer.path_placeholder')}
            />
          </div>

          <button class="btn btn-primary btn-sm" onclick={() => navigateToPath(pathInput)} disabled={loading}>
            {$t('dashboard.file_explorer.go_button')}
          </button>
        </div>

        <div class="flex items-center justify-between gap-2">
          <div class="flex items-center gap-1">
            {#if hasSelectedFiles}
              <button class="btn btn-ghost btn-xs" onclick={clearSelection} in:fade>
                {$t('dashboard.file_explorer.clear_selection')}
              </button>
            {/if}
            <button class="btn btn-ghost btn-xs" onclick={selectAll} disabled={fileManagerData.length === 0}>
              {$t('dashboard.file_explorer.select_all')}
            </button>
          </div>

          <div class="text-xs text-base-content/60 truncate">
            {$t('dashboard.file_explorer.current_path', { values: { path: currentPath } })}
          </div>
        </div>
      </div>

      <!-- File browser -->
      <div class="flex-1 overflow-hidden relative">
        {#if loading}
          <div class="absolute inset-0 bg-base-100/70 flex items-center justify-center z-10" transition:fade={{ speed: "fast" }}>
            <div class="flex items-center gap-2">
              <Loader class="w-5 h-5 animate-spin text-primary" />
              <span class="text-sm text-base-content/70">{$t('dashboard.file_explorer.loading')}</span>
            </div>
          </div>
        {/if}

        <div class="h-full overflow-y-auto selectable">
          {#if fileManagerData.length === 0 && !loading}
            <div class="flex items-center justify-center h-full text-base-content/65">
              <div class="text-center">
                <Folder class="w-10 h-10 mx-auto mb-3 opacity-40" />
                <p class="text-sm">{$t('dashboard.file_explorer.empty_directory')}</p>
              </div>
            </div>
          {:else}
            <div>
              {#each fileManagerData as item (item.id)}
                {@const isSelected = selectedFiles.has(item.id)}
                <div
                  class="flex items-center gap-3 px-3 py-1.5 border-t border-[var(--ui-hairline)] first:border-t-0 transition-colors hover:bg-base-200/60"
                  style={isSelected ? "background-color: color-mix(in oklab, var(--color-primary) 8%, transparent)" : ""}
                >
                  <div class="shrink-0 text-base-content/60">
                    {#if item.type === "folder"}
                      <Folder class="w-5 h-5 text-primary" />
                    {:else}
                      <FileIcon class="w-5 h-5" />
                    {/if}
                  </div>

                  <button
                    onclick={() => toggleSelected(item.id)}
                    ondblclick={() => {
                      // Double-click navigates into folders
                      if (item.type === "folder") {
                        navigateToPath(item.id);
                      }
                    }}
                    class="flex-1 text-left min-w-0 {isSelected ? 'text-primary' : 'text-base-content'}"
                  >
                    <div class="text-sm font-medium truncate">{item.id.split('/').pop()}</div>
                    <div class="text-xs text-base-content/60 flex items-center gap-2 tabular-nums">
                      <span>
                        {item.type === "file"
                          ? $t('dashboard.file_explorer.file_size_kb', { values: { size: (item.size / 1024).toFixed(1) } })
                          : $t('dashboard.file_explorer.folder_type')
                        }
                      </span>
                      <span>•</span>
                      <span>{item.date.toLocaleDateString()}</span>
                    </div>
                  </button>

                  <input
                    type="checkbox"
                    class="checkbox checkbox-sm checkbox-primary shrink-0"
                    checked={isSelected}
                    onchange={() => toggleSelected(item.id)}
                  />
                </div>
              {/each}
            </div>
          {/if}
        </div>
      </div>

      <!-- Footer -->
      <div class="flex items-center justify-between gap-3 px-4 py-3 border-t border-[var(--ui-hairline)]">
        <div class="text-xs text-base-content/60 tabular-nums">
          {#if hasSelectedFiles}
            {$t('dashboard.file_explorer.items_selected', { values: { count: selectedFileCount } })}
          {:else}
            {$t('dashboard.file_explorer.item_count_footer', { values: { count: fileManagerData.length } })}
          {/if}
        </div>

        <div class="flex gap-2">
          <button class="btn btn-ghost btn-sm" onclick={onClose}>
            {$t('dashboard.file_explorer.cancel_button')}
          </button>

          <button class="btn btn-primary btn-sm" onclick={importSelectedFiles} disabled={!hasSelectedFiles || importing}>
            {#if importing}
              <Loader class="w-4 h-4 animate-spin" />
              {$t('dashboard.file_explorer.adding_to_queue')}
            {:else}
              <Upload class="w-4 h-4" />
              {$t('dashboard.file_explorer.add_to_queue_button', { values: { count: selectedFileCount } })}
            {/if}
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}
