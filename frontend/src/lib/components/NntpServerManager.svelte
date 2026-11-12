<script lang="ts">
  import apiClient from "$lib/api/client";
  import { t } from "$lib/i18n";
  import { toastStore } from "$lib/stores/toast";
  import { config as configType } from "$lib/wailsjs/go/models";
  import { Check, ChevronDown, CirclePlus, Info, Loader2, Server, Trash2 } from "lucide-svelte";
  import FieldRow from "$lib/components/inputs/FieldRow.svelte";
  import { collapse } from "$lib/components/settings/collapse";
  import { fade, fly, listFlip, scale } from "$lib/motion";

  interface ValidationState {
    status: "pending" | "validating" | "valid" | "invalid" | "incomplete";
    error: string;
  }

  interface Props {
    servers: configType.ServerConfig[];
    onupdate?: (servers: configType.ServerConfig[]) => void;
    onvalidationchange?: (data: { hasValidServers: boolean }) => void;
    showAdvancedFields?: boolean;
    variant?: "setup" | "settings";
    restrictedRole?: "upload" | "verify";
  }

  let {
    servers = $bindable([]),
    onupdate,
    onvalidationchange,
    showAdvancedFields = false,
    variant = "setup",
    restrictedRole,
  }: Props = $props();

  // Track validation state for each server
  let validationStates = $state<Record<number, ValidationState>>({});

  // Local reactive state for server properties
  let localServers = $state<configType.ServerConfig[]>([]);

  // Initialize local state from props only once
  $effect(() => {
    if (localServers.length === 0 && servers.length > 0) {
      localServers = servers.map((server) => ({ ...server }));
    }
  });

  // Stable identity per server object so rows can be keyed (enter/leave/reorder
  // motion) while validation state keeps being tracked by index.
  const serverIds = new WeakMap<object, number>();
  const defaultOpen = new Map<number, boolean>();
  let nextServerId = 0;
  let userToggled = $state<Record<number, boolean>>({});

  function idFor(server: configType.ServerConfig): number {
    let id = serverIds.get(server);
    if (id === undefined) {
      id = nextServerId++;
      serverIds.set(server, id);
      // New or empty servers start expanded so they can be filled in right away;
      // so does a lone server (nothing else competes for space).
      defaultOpen.set(id, !server.host || localServers.length === 1);
    }
    return id;
  }

  function isOpen(id: number, _server: configType.ServerConfig): boolean {
    return userToggled[id] ?? defaultOpen.get(id) ?? false;
  }

  function toggleOpen(id: number, server: configType.ServerConfig): void {
    userToggled = { ...userToggled, [id]: !isOpen(id, server) };
  }

  // Update servers prop when local state changes
  function updateServers(): void {
    servers = localServers.map((server) => ({ ...server }));
    onupdate?.(servers);
  }

  function addServer(): void {
    const newServer = new configType.ServerConfig({
      max_connections: 10,
      inflight: 10,
      max_connection_idle_time_in_seconds: 300,
      max_connection_ttl_in_seconds: 3600,
      insecure_ssl: false,
    });

    if (restrictedRole) {
      newServer.role = restrictedRole;
    }

    localServers = [...localServers, newServer];
    const newIndex = localServers.length - 1;

    validationStates = {
      ...validationStates,
      [newIndex]: { status: "pending", error: "" },
    };

    updateServers();
  }

  function removeServer(index: number): void {
    // Prevent removing the last upload server
    if (restrictedRole === "upload" && localServers.length <= 1) {
      toastStore.error($t("settings.server.validation.cannot_disable_last"));
      return;
    }

    localServers = localServers.filter((_, i) => i !== index);

    // Rebuild validation states with new indices
    const newValidationStates: Record<number, ValidationState> = {};
    let newIndex = 0;
    for (let i = 0; i < localServers.length + 1; i++) {
      if (i === index || !validationStates[i]) {
        continue;
      }
      newValidationStates[newIndex] = validationStates[i];
      newIndex++;
    }
    validationStates = newValidationStates;
    updateServers();
  }

  function onServerFieldChange(index: number): void {
    // Clear validation state when server data changes
    const currentState = validationStates[index];
    if (currentState && currentState.status !== "pending") {
      validationStates = {
        ...validationStates,
        [index]: { status: "pending", error: "" },
      };
    }
    updateServers();
  }

  function onServerEnabledChange(index: number, enabled: boolean): void {
    // If trying to disable, check if it's the last enabled server
    if (!enabled) {
      const enabledCount = localServers.filter(s => s.enabled !== false).length;
      if (enabledCount <= 1) {
        toastStore.error($t("settings.server.validation.cannot_disable_last"));
        // Revert the change
        localServers[index].enabled = true;
        return;
      }
    }

    localServers[index].enabled = enabled;
    onServerFieldChange(index);
  }

  function onServerRoleChange(index: number, role: string): void {
    // If switching to verify, ensure at least one upload server will remain
    if (role === "verify") {
      const uploadServersCount = localServers.filter((s, i) => {
        const isEnabled = s.enabled !== false;
        const effectiveRole = i === index ? "verify" : (s.role || "upload");
        return isEnabled && effectiveRole !== "verify";
      }).length;

      if (uploadServersCount < 1) {
        toastStore.error($t("settings.server.validation.cannot_set_all_verify"));
        // Revert the change
        localServers[index].role = "upload";
        return;
      }
    }

    localServers[index].role = role;
    onServerFieldChange(index);
  }

  function getServerValidationState(index: number): ValidationState {
    const state = validationStates[index];
    if (!state) {
      return { status: "pending", error: "" };
    }
    return state;
  }

  function isServerComplete(index: number): boolean {
    const validationState = getServerValidationState(index);
    return validationState.status === "valid";
  }

  // Check if any servers are valid and emit validation state
  function checkValidationState(): boolean {
    const hasValid = localServers.some((_, index) => isServerComplete(index));
    onvalidationchange?.({ hasValidServers: hasValid });
    return hasValid;
  }

  // Reactive effect to automatically check validation state when validation states change
  $effect(() => {
    // This will run whenever validationStates changes
    checkValidationState();
  });

  // Bulk operations
  function enableAllServers(): void {
    localServers.forEach((server, index) => {
      if (!server.enabled) {
        localServers[index].enabled = true;
        onServerFieldChange(index);
      }
    });
  }

  function disableAllServers(): void {
    // Ensure at least one server remains enabled
    const enabledCount = localServers.filter(s => s.enabled !== false).length;
    if (enabledCount <= 1) {
      toastStore.error($t("settings.server.bulk_operations.cannot_disable_all"));
      return;
    }

    localServers.forEach((server, index) => {
      if (server.enabled !== false) {
        localServers[index].enabled = false;
        onServerFieldChange(index);
      }
    });
  }

  // Computed values for bulk operations
  let enabledServersCount = $derived(localServers.filter(s => s.enabled !== false).length);
  let totalServersCount = $derived(localServers.length);
  let hasDisabledServers = $derived(localServers.some(s => s.enabled === false));

  async function validateServer(index: number): Promise<void> {
    const server = localServers[index];
    if (!server) {
      return;
    }

    // Basic validation first
    const host = server.host;
    const port = server.port;

    if (!host || !port) {
      validationStates = {
        ...validationStates,
        [index]: { status: "incomplete", error: "Host and port are required" },
      };
      checkValidationState();
      return;
    }

    validationStates = {
      ...validationStates,
      [index]: { status: "validating", error: "" },
    };

    try {
      const result = await apiClient.validateNNTPServer({
        name: server.name || "",
        host,
        port,
        username: server.username || "",
        password: server.password || "",
        ssl: server.ssl || false,
        maxConnections: server.max_connections || 10,
        role: server.role || "upload",
      });

      if (result.valid) {
        validationStates = {
          ...validationStates,
          [index]: { status: "valid", error: "" },
        };
        toastStore.success($t("settings.server.valid"));
        checkValidationState();
        return;
      }

      validationStates = {
        ...validationStates,
        [index]: { status: "invalid", error: result.error },
      };
      toastStore.error($t("settings.server.invalid"), String(result.error));
    } catch (error) {
      const errorMessage =
        error instanceof Error ? error.message : String(error);
      validationStates = {
        ...validationStates,
        [index]: {
          status: "invalid",
          error: `Validation failed: ${errorMessage}`,
        },
      };
      toastStore.error($t("settings.server.invalid"), String(errorMessage));
      console.error("Server validation error:", errorMessage);
    }

    checkValidationState();
  }

  // Add default server if none exist (only for setup variant)
  $effect(() => {
    if (
      variant === "setup" &&
      localServers.length === 0 &&
      servers.length === 0
    ) {
      addServer();
    }
  });
</script>

{#snippet statusDot(state: ValidationState, enabled: boolean)}
  <span
    class="status-dot"
    class:status-dot-valid={enabled && state.status === "valid"}
    class:status-dot-error={enabled && (state.status === "invalid" || state.status === "incomplete")}
    class:status-dot-busy={enabled && state.status === "validating"}
    class:status-dot-off={!enabled}
    aria-hidden="true"
  ></span>
{/snippet}

<div class="space-y-4">
  {#if variant === "setup"}
    <div>
      <h3 class="text-lg font-semibold text-base-content mb-1">
        {$t("settings.server.title")}
      </h3>
      <p class="text-base-content/70">
        {$t("settings.server.description")}
      </p>
    </div>
  {/if}

  <!-- Status and Bulk Operations (settings variant, no restrictedRole) -->
  {#if localServers.length > 0 && variant === "settings" && !restrictedRole}
    <div class="settings-group" transition:collapse>
      <div class="settings-row">
        <div>
          <div class="settings-row-title">Providers Status</div>
          <div class="settings-row-desc">
            <span class="tabular-nums">{enabledServersCount} / {totalServersCount}</span>
            {$t("settings.server.status.providers_active")}
          </div>
        </div>
        <div class="flex gap-2">
          {#if hasDisabledServers}
            <button class="btn btn-sm" onclick={enableAllServers} transition:fade={{ speed: "fast" }}>
              {$t("settings.server.bulk_operations.enable_all")}
            </button>
          {/if}
          {#if enabledServersCount > 1}
            <button class="btn btn-sm" onclick={disableAllServers} transition:fade={{ speed: "fast" }}>
              {$t("settings.server.bulk_operations.disable_all")}
            </button>
          {/if}
        </div>
      </div>
    </div>
  {/if}

  {#if localServers.length === 0}
    {#if restrictedRole === "verify"}
      <!-- Empty verify pool notice -->
      <div class="settings-group" in:fade={{ speed: "base" }}>
        <div class="settings-row justify-start gap-3">
          <Info class="w-4 h-4 text-base-content/60 shrink-0" />
          <p class="text-sm text-base-content/70">
            {$t("settings.server.verify_pool_empty_notice")}
          </p>
        </div>
      </div>
      <button type="button" class="btn btn-sm" onclick={addServer}>
        <CirclePlus class="w-4 h-4" />
        {$t("settings.server.add_verify_server")}
      </button>
    {:else}
      <div class="settings-group" in:fade={{ speed: "base" }}>
        <div class="flex flex-col items-center gap-3 px-4 py-8 text-center">
          <Server class="w-8 h-8 text-base-content/40" />
          <p class="text-sm text-base-content/70">
            {$t("settings.server.no_servers_description")}
          </p>
          <button type="button" class="btn btn-sm" onclick={addServer}>
            <CirclePlus class="w-4 h-4" />
            {$t("settings.server.add_first_server")}
          </button>
        </div>
      </div>
    {/if}
  {:else}
    {#each localServers as server, index (idFor(server))}
      {@const sid = idFor(server)}
      {@const validationState = getServerValidationState(index)}
      {@const isEnabled = server.enabled !== false}
      {@const open = isOpen(sid, server)}
      <div
        class="settings-group"
        animate:listFlip
        in:fly={{ y: 10 }}
        out:fade={{ speed: "fast" }}
      >
        <!-- Summary row -->
        <div class="settings-row server-summary" class:opacity-60={!isEnabled}>
          <button
            type="button"
            class="server-toggle"
            aria-expanded={open}
            aria-controls="server-panel-{sid}"
            onclick={() => toggleOpen(sid, server)}
          >
            {@render statusDot(validationState, isEnabled)}
            <span class="min-w-0 flex-1 text-left">
              <span class="settings-row-title block truncate">
                {#if server.name}
                  {server.name}
                {:else if restrictedRole === "upload"}
                  {$t("settings.server.account_number", { values: { number: index + 1 } })}
                {:else}
                  {$t("settings.server.server_number", { values: { number: index + 1 } })}
                {/if}
              </span>
              {#if server.host}
                <span class="settings-row-desc block truncate">
                  {server.host}{server.port ? `:${server.port}` : ""}{server.username ? ` · ${server.username}` : ""}
                </span>
              {/if}
            </span>
          </button>

          <div class="flex shrink-0 items-center gap-1.5">
            {#if !isEnabled}
              <span class="badge badge-ghost badge-sm" in:fade={{ speed: "fast" }}>
                {$t("settings.server.disabled")}
              </span>
            {/if}
            {#key validationState.status}
              {#if validationState.status === "validating"}
                <span class="inline-flex items-center gap-1 text-xs text-base-content/70" in:fade={{ speed: "fast" }}>
                  <Loader2 class="w-3 h-3 animate-spin" />
                  {$t("settings.server.validating")}
                </span>
              {:else if validationState.status === "valid"}
                <span class="inline-flex items-center gap-1 text-xs text-success" in:scale={{ speed: "fast", start: 0.85 }}>
                  <Check class="w-3 h-3" />
                  {$t("settings.server.valid")}
                </span>
              {:else if validationState.status === "invalid"}
                <span class="text-xs text-error" in:fade={{ speed: "fast" }}>
                  {$t("settings.server.invalid")}
                </span>
              {:else if validationState.status === "incomplete"}
                <span class="text-xs text-error" in:fade={{ speed: "fast" }}>
                  {$t("settings.server.incomplete")}
                </span>
              {/if}
            {/key}
            {#if validationState.status !== "validating"}
              <button class="btn btn-sm" onclick={() => validateServer(index)}>
                {$t("settings.server.test_connection")}
              </button>
            {/if}
            {#if restrictedRole === "upload" ? localServers.length > 1 : localServers.length > 0}
              <button
                class="btn btn-sm btn-ghost btn-square text-error"
                onclick={() => removeServer(index)}
                aria-label={$t("settings.server.remove")}
                title={$t("settings.server.remove")}
              >
                <Trash2 class="w-4 h-4" />
              </button>
            {/if}
            <ChevronDown
              class="w-4 h-4 text-base-content/50 transition-transform duration-(--motion-base) {open ? 'rotate-180' : ''}"
            />
          </div>
        </div>

        {#if validationState.error}
          <div transition:collapse class="border-t border-(--ui-hairline)" role="alert">
            <p class="px-4 py-2.5 text-sm text-error">{validationState.error}</p>
          </div>
        {/if}

        {#if open}
          <div id="server-panel-{sid}" transition:collapse class="border-t border-(--ui-hairline)">
            {#if variant === "settings" && server.enabled !== undefined}
              <FieldRow
                label={$t("settings.server.enabled")}
                description={isEnabled
                  ? $t("settings.server.enabled_description")
                  : $t("settings.server.disabled_description")}
                id="server-enabled-{index}"
              >
                <input
                  id="server-enabled-{index}"
                  type="checkbox"
                  class="toggle toggle-primary"
                  checked={localServers[index].enabled !== false}
                  onchange={(e) => onServerEnabledChange(index, (e.target as HTMLInputElement).checked)}
                />
              </FieldRow>
            {/if}

            <!-- Server Role selector — only shown in legacy/setup mode (no restrictedRole) -->
            {#if !restrictedRole}
              <FieldRow
                label={$t("settings.server.role")}
                description={(localServers[index].role || "upload") === "verify"
                  ? $t("settings.server.role_verify_description")
                  : $t("settings.server.role_upload_description")}
                id="server-role-{index}"
              >
                <div class="flex items-center gap-2">
                  {#if (localServers[index].role || "upload") === "verify"}
                    <span class="badge badge-ghost badge-sm" in:fade={{ speed: "fast" }}>
                      {$t("settings.server.role_verify_badge")}
                    </span>
                  {/if}
                  <select
                    id="server-role-{index}"
                    class="select select-sm w-auto"
                    value={localServers[index].role || "upload"}
                    onchange={(e) => onServerRoleChange(index, (e.target as HTMLSelectElement).value)}
                  >
                    <option value="upload">{$t("settings.server.role_upload")}</option>
                    <option value="verify">{$t("settings.server.role_verify")}</option>
                  </select>
                </div>
              </FieldRow>
            {/if}

            <FieldRow label={$t("settings.server.name")} id="name-{index}">
              <input
                id="name-{index}"
                class="input input-sm w-64 max-sm:w-full"
                bind:value={localServers[index].name}
                placeholder={$t("settings.server.name_placeholder")}
                oninput={() => onServerFieldChange(index)}
              />
            </FieldRow>

            <FieldRow label={`${$t("settings.server.host")} *`} id="host-{index}">
              <input
                id="host-{index}"
                class="input input-sm w-64 max-sm:w-full"
                bind:value={localServers[index].host}
                placeholder="news.example.com"
                required
                oninput={() => onServerFieldChange(index)}
              />
            </FieldRow>

            <FieldRow label={`${$t("settings.server.port")} *`} id="port-{index}">
              <input
                id="port-{index}"
                class="input input-sm w-28 text-right tabular-nums"
                type="number"
                bind:value={localServers[index].port}
                min="1"
                max="65535"
                required
                oninput={() => onServerFieldChange(index)}
              />
            </FieldRow>

            <FieldRow label={$t("settings.server.use_ssl_tls")} id="ssl-{index}">
              <input
                id="ssl-{index}"
                type="checkbox"
                class="toggle toggle-primary"
                bind:checked={localServers[index].ssl}
                onchange={() => onServerFieldChange(index)}
              />
            </FieldRow>

            {#if variant === "settings" && showAdvancedFields}
              <div transition:collapse>
                <FieldRow label={$t("settings.server.allow_insecure_ssl")} id="insecure-ssl-{index}">
                  <input
                    id="insecure-ssl-{index}"
                    type="checkbox"
                    class="toggle toggle-primary"
                    bind:checked={localServers[index].insecure_ssl}
                    onchange={() => onServerFieldChange(index)}
                  />
                </FieldRow>
              </div>
            {/if}

            <FieldRow label={$t("settings.server.username")} id="username-{index}">
              <input
                id="username-{index}"
                class="input input-sm w-64 max-sm:w-full"
                autocomplete="off"
                bind:value={localServers[index].username}
                oninput={() => onServerFieldChange(index)}
              />
            </FieldRow>

            <FieldRow label={$t("settings.server.password")} id="password-{index}">
              <input
                id="password-{index}"
                class="input input-sm w-64 max-sm:w-full"
                type="password"
                autocomplete="new-password"
                bind:value={localServers[index].password}
                oninput={() => onServerFieldChange(index)}
              />
            </FieldRow>

            <FieldRow label={$t("settings.server.max_connections")} id="maxConnections-{index}">
              <input
                id="maxConnections-{index}"
                class="input input-sm w-28 text-right tabular-nums"
                type="number"
                bind:value={localServers[index].max_connections}
                min="1"
                max="50"
                oninput={() => onServerFieldChange(index)}
              />
            </FieldRow>

            {#if variant === "settings" && showAdvancedFields}
              <div transition:collapse>
                <FieldRow
                  label={$t("settings.server.inflight")}
                  description={$t("settings.server.inflight_description")}
                  id="inflight-{index}"
                >
                  <input
                    id="inflight-{index}"
                    class="input input-sm w-28 text-right tabular-nums"
                    type="number"
                    bind:value={localServers[index].inflight}
                    min="0"
                    max="100"
                    placeholder="0"
                    oninput={() => onServerFieldChange(index)}
                  />
                </FieldRow>
                <FieldRow
                  label={$t("settings.server.connection_idle_timeout")}
                  description={$t("settings.server.connection_idle_timeout_description")}
                  id="idle-time-{index}"
                >
                  <input
                    id="idle-time-{index}"
                    type="number"
                    class="input input-sm w-28 text-right tabular-nums"
                    bind:value={localServers[index].max_connection_idle_time_in_seconds}
                    min="1"
                    max="3600"
                    placeholder="300"
                    onchange={() => onServerFieldChange(index)}
                  />
                </FieldRow>
                <FieldRow
                  label={$t("settings.server.connection_ttl")}
                  description={$t("settings.server.connection_ttl_description")}
                  id="ttl-{index}"
                >
                  <input
                    id="ttl-{index}"
                    type="number"
                    class="input input-sm w-28 text-right tabular-nums"
                    bind:value={localServers[index].max_connection_ttl_in_seconds}
                    min="1"
                    max="86400"
                    placeholder="3600"
                    onchange={() => onServerFieldChange(index)}
                  />
                </FieldRow>
              </div>
            {/if}

            <FieldRow
              label={$t("settings.server.proxy.url_label")}
              description={$t("settings.server.proxy.url_description")}
              id="proxy-url-{index}"
            >
              <input
                id="proxy-url-{index}"
                type="text"
                class="input input-sm w-64 max-sm:w-full"
                bind:value={localServers[index].proxy_url}
                placeholder="socks5://user:pass@proxy.example.com:1080"
                oninput={() => onServerFieldChange(index)}
              />
            </FieldRow>
          </div>
        {/if}
      </div>
    {/each}

    <button class="btn btn-sm" onclick={addServer}>
      <CirclePlus class="w-4 h-4" />
      {#if restrictedRole === "upload"}
        {$t("settings.server.add_account")}
      {:else if restrictedRole === "verify"}
        {$t("settings.server.add_verify_server")}
      {:else}
        {$t("settings.server.add_server")}
      {/if}
    </button>
  {/if}
</div>

<style>
  .server-toggle {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex: 1 1 0;
    min-width: 0;
    cursor: pointer;
    text-align: left;
  }
  .status-dot {
    flex: none;
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 9999px;
    background: color-mix(in oklab, var(--color-base-content) 30%, transparent);
    transition: background-color var(--motion-base) var(--ease-standard);
  }
  .status-dot-valid {
    background: var(--color-success);
  }
  .status-dot-error {
    background: var(--color-error);
  }
  .status-dot-busy {
    background: var(--color-primary);
    animation: pulse-soft 1.2s var(--ease-standard) infinite;
  }
  .status-dot-off {
    background: color-mix(in oklab, var(--color-base-content) 18%, transparent);
  }
  @media (max-width: 40rem) {
    .server-summary {
      flex-wrap: wrap;
    }
  }
</style>
