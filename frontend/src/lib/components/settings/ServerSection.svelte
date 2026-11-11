<script lang="ts">
import NntpServerManager from "$lib/components/NntpServerManager.svelte";
import { t } from "$lib/i18n";
import { advancedMode } from "$lib/stores/app";
import { config as configType } from "$lib/wailsjs/go/models";
import { AlertTriangle } from "lucide-svelte";
import { collapse } from "./collapse";

interface Props {
	config: configType.ConfigData;
}

let { config = $bindable() }: Props = $props();

let isAdvanced = $derived($advancedMode);

let hasMultipleUploadBackbones = $derived(
	(() => {
		const hosts = new Set(
			(config.servers ?? [])
				.filter(s => s != null && (s.role || "upload") !== "verify" && s.enabled !== false)
				.map(s => s.host)
				.filter(h => h)
		);
		return hosts.size > 1;
	})()
);

// Helper to build a ServerConfig from a plain object
function toServerConfig(server: any, role: "upload" | "verify"): configType.ServerConfig {
	const s = new configType.ServerConfig();
	s.name = server.name || "";
	s.enabled = server.enabled ?? true;
	s.host = server.host || "";
	s.port = server.port || 119;
	s.username = server.username || "";
	s.password = server.password || "";
	s.max_connections = server.max_connections || 10;
	s.inflight = server.inflight || 10;
	s.ssl = server.ssl ?? false;
	s.insecure_ssl = server.insecure_ssl ?? false;
	s.max_connection_idle_time_in_seconds = server.max_connection_idle_time_in_seconds || 300;
	s.max_connection_ttl_in_seconds = server.max_connection_ttl_in_seconds || 3600;
	s.role = role;
	s.proxy_url = server.proxy_url || "";
	return s;
}

// Derive filtered lists to pass to each NntpServerManager
let uploadManagedServers = $derived(
	(config.servers ?? [])
		.filter(s => s != null && (s.role || "upload") !== "verify")
		.map(s => ({
			name: s.name || "",
			enabled: s.enabled ?? true,
			host: s.host || "",
			port: s.port || 119,
			username: s.username || "",
			password: s.password || "",
			max_connections: s.max_connections || 10,
			ssl: s.ssl ?? false,
			insecure_ssl: s.insecure_ssl ?? false,
			max_connection_idle_time_in_seconds: s.max_connection_idle_time_in_seconds || 300,
			max_connection_ttl_in_seconds: s.max_connection_ttl_in_seconds || 3600,
			role: "upload" as const,
			inflight: s.inflight || 10,
			proxy_url: s.proxy_url || "",
		}))
);

let verifyManagedServers = $derived(
	(config.servers ?? [])
		.filter(s => s != null && s.role === "verify")
		.map(s => ({
			name: s.name || "",
			enabled: s.enabled ?? true,
			host: s.host || "",
			port: s.port || 119,
			username: s.username || "",
			password: s.password || "",
			max_connections: s.max_connections || 10,
			ssl: s.ssl ?? false,
			insecure_ssl: s.insecure_ssl ?? false,
			max_connection_idle_time_in_seconds: s.max_connection_idle_time_in_seconds || 300,
			max_connection_ttl_in_seconds: s.max_connection_ttl_in_seconds || 3600,
			role: "verify" as const,
			inflight: s.inflight || 10,
			proxy_url: s.proxy_url || "",
		}))
);

function handleUploadUpdate(updatedServers: any[]) {
	config.servers = [
		...updatedServers.map(s => toServerConfig(s, "upload")),
		...(config.servers ?? []).filter(s => s?.role === "verify").map(s => toServerConfig(s, "verify")),
	];
}

function handleVerifyUpdate(updatedServers: any[]) {
	config.servers = [
		...(config.servers ?? []).filter(s => s != null && (s.role || "upload") !== "verify").map(s => toServerConfig(s, "upload")),
		...updatedServers.map(s => toServerConfig(s, "verify")),
	];
}

</script>

<div class="space-y-6">
  <!-- Upload Pool -->
  <section>
    <div class="section-label">{$t("settings.server.upload_pool_title")}</div>
    <p class="settings-row-desc mx-1 mb-2">{$t("settings.server.upload_pool_description")}</p>

    {#if hasMultipleUploadBackbones}
      <div transition:collapse role="status">
        <div class="mb-3 flex items-start gap-3 rounded-(--radius-box) border border-warning/40 bg-warning/10 px-4 py-3">
          <AlertTriangle class="mt-0.5 h-4 w-4 shrink-0 text-warning-ink" />
          <span class="text-sm">{$t("settings.server.multiple_backbones_warning")}</span>
        </div>
      </div>
    {/if}

    <NntpServerManager
      servers={uploadManagedServers}
      onupdate={handleUploadUpdate}
      showAdvancedFields={isAdvanced}
      variant="settings"
      restrictedRole="upload"
    />
  </section>

  <!-- Verify Pool -->
  <section>
    <div class="section-label">
      {$t("settings.server.verify_pool_title")}
      <span class="badge badge-ghost badge-sm ml-1 normal-case tracking-normal">
        {$t("settings.server.verify_pool_optional")}
      </span>
    </div>
    <p class="settings-row-desc mx-1 mb-2">{$t("settings.server.verify_pool_description")}</p>

    <NntpServerManager
      servers={verifyManagedServers}
      onupdate={handleVerifyUpdate}
      showAdvancedFields={isAdvanced}
      variant="settings"
      restrictedRole="verify"
    />
  </section>

  <p class="settings-row-desc mx-1">
    {@html $t("settings.server.tip")}
  </p>
</div>
