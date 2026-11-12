<script lang="ts">
import { afterNavigate, goto } from "$app/navigation";
import { page } from "$app/state";
import apiClient from "$lib/api/client";
import DragStrip from "$lib/components/shell/DragStrip.svelte";
import Sidebar from "$lib/components/shell/Sidebar.svelte";
import Toolbar from "$lib/components/shell/Toolbar.svelte";
import ToastContainer from "$lib/components/ToastContainer.svelte";
import { t } from "$lib/i18n";
import { fly } from "$lib/motion";
import { appStatus } from "$lib/stores/app";
import { initPlatform } from "$lib/stores/platform";
import { toastStore } from "$lib/stores/toast";
import { AlertCircle } from "lucide-svelte";
import { onMount, onDestroy } from "svelte";
import "$lib/stores/theme"; // registers the OS colour-scheme listener on every route
import type { Snippet } from "svelte";

interface Props {
	children: Snippet;
}

let { children }: Props = $props();
import "../style.css";

let authReady = $state(false);
let authError = $state("");
let webAuthenticated = $state(false);
let needsConfiguration = $state(false);
let criticalConfigError = $state(false);
let connectionStatus: "connected" | "reconnecting" | "disconnected" = $state("connected");
let connectionCheckInterval: ReturnType<typeof setInterval> | undefined;
let drawerOpen = $state(false);
let scrollPane = $state<HTMLElement>();

// Setup and login render full screen, without the sidebar shell.
let plainRoute = $derived(page.route.id === "/setup" || page.route.id === "/login");

afterNavigate(() => {
	// The content pane (not the window) scrolls, so reset it per navigation.
	scrollPane?.scrollTo({ top: 0, behavior: "instant" });
	drawerOpen = false;
});

onMount(async () => {
	try {
		await initPlatform();

		// Gate child pages before their onMount hooks can call privileged APIs.
		const desktop = !!(window as unknown as {go?: {backend?: {App?: unknown}}}).go?.backend?.App;
		if (page.route.id === "/login") { authReady = true; return; }
		if (!desktop) {
			const response = await fetch("/api/auth/user", { signal: AbortSignal.timeout(15000) });
			if (response.status === 401) { window.location.replace("/login"); return; }
			if (!response.ok) throw new Error($t("common.auth.connectionError"));
			webAuthenticated = (await response.json()).enabled;
		}
		authReady = true;

		// Initialize API client (detects environment and sets up appropriate client)
		await apiClient.initialize();

		// Listen for config updates
		await apiClient.on("config-updated", async () => {
			await loadAppStatus();
		});

		// Listen for menu navigation events (desktop only)
		if (apiClient.environment === "wails") {
			await apiClient.on("navigate-to-settings", () => {
				goto("/settings");
			});

			await apiClient.on("navigate-to-dashboard", () => {
				goto("/");
			});

			// Listen for edit menu events (desktop only)
			await apiClient.on("menu-cut", () => {
				document.execCommand("cut");
			});

			await apiClient.on("menu-copy", () => {
				document.execCommand("copy");
			});

			await apiClient.on("menu-paste", () => {
				document.execCommand("paste");
			});

			await apiClient.on("menu-undo", () => {
				document.execCommand("undo");
			});

			await apiClient.on("menu-redo", () => {
				document.execCommand("redo");
			});

			await apiClient.on("menu-select-all", () => {
				document.execCommand("selectAll");
			});
		}

		// Load initial app status
		await loadAppStatus();

		// Periodic connection health check
		connectionCheckInterval = setInterval(async () => {
			try {
				await apiClient.getAppStatus();
				if (connectionStatus !== "connected") connectionStatus = "connected";
			} catch {
				connectionStatus = connectionStatus === "connected" ? "reconnecting" : "disconnected";
			}
		}, 10000);
	} catch (error) {
		console.error("Failed to initialize app:", error);
		authError = $t("common.auth.connectionError");
		toastStore.error($t("common.common.error"), (error as Error).message);
	}
});

async function loadAppStatus() {
	try {
		const status = await apiClient.getAppStatus();
		appStatus.set(status);
		needsConfiguration = status.needsConfiguration || false;
		criticalConfigError = status.criticalConfigError || false;

		// Force redirect to setup wizard if this is first start and not already on setup page
		if (status.isFirstStart && page.route.id !== "/setup") {
			goto("/setup");
			return;
		}

		// Auto-redirect to settings if there's a critical configuration error
		if (
			criticalConfigError &&
			page.route.id !== "/settings" &&
			page.route.id !== "/setup"
		) {
			toastStore.error(
				$t("common.common.error"),
				$t("common.messages.error_saving"),
			);
			goto("/settings");
		}
	} catch (error) {
		console.error("Failed to load app status:", error);
		// Do not redirect to setup on error — wizard must only show on first start
	}
}

onDestroy(() => {
	if (connectionCheckInterval) clearInterval(connectionCheckInterval);
});

async function logout() {
	try {
		const response = await fetch("/api/auth/logout", { method: "POST", headers: { "Content-Type": "application/json" } });
		if (!response.ok) throw new Error($t("common.auth.logoutError"));
		window.location.replace("/login");
	} catch (error) { toastStore.error($t("common.auth.logoutError"), (error as Error).message); }
}

function handleKeydown(event: KeyboardEvent) {
	if (event.key === "Escape" && drawerOpen) {
		drawerOpen = false;
		return;
	}
	// Skip if user is typing in an input
	const target = event.target as HTMLElement;
	if (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable) return;

	const mod = event.metaKey || event.ctrlKey;
	if (!mod) return;

	const routes: Record<string, string> = {
		"1": "/",
		"2": "/settings",
		"3": "/metrics",
		"4": "/logs",
	};

	const route = routes[event.key];
	if (route) {
		event.preventDefault();
		goto(route);
	}
}

function handler(error: unknown, _reset: () => void) {
	const err = error as Error;
	console.error("Error in layout:", err);
	toastStore.error($t("common.common.error"), err.message);
}
</script>

<svelte:window
	onkeydown={handleKeydown}
	onresize={() => {
		if (drawerOpen && window.innerWidth >= 640) drawerOpen = false;
	}}
/>

{#if authReady}
<div class="app-root" class:plain={plainRoute}>
	<svelte:boundary onerror={handler}>
		{#if plainRoute}
			<div class="shell-plain">
				<DragStrip>
					{#if webAuthenticated && page.route.id === "/setup"}
						<button class="btn btn-sm btn-ghost" data-testid="sign-out" onclick={logout}>{$t("common.auth.signOut")}</button>
					{/if}
				</DragStrip>
				{@render children()}
			</div>
		{:else}
			<div class="app-shell">
				<Sidebar
					activeRoute={page.route.id}
					configBlocked={needsConfiguration || criticalConfigError}
					connection={connectionStatus}
					version={$appStatus?.version}
					{webAuthenticated}
					{drawerOpen}
					onLogout={logout}
					onClose={() => (drawerOpen = false)}
				/>
				<div class="shell-main" inert={drawerOpen}>
					<Toolbar activeRoute={page.route.id} onMenu={() => (drawerOpen = true)} />
					<main class="shell-scroll" bind:this={scrollPane}>
						{#key page.route.id}
							<div class="shell-page" in:fly={{ y: 6 }}>
								{@render children()}
							</div>
						{/key}
					</main>
				</div>
			</div>
		{/if}
	{#snippet failed(error)}
		<div class="flex h-full flex-col items-center justify-center text-center space-y-6 p-6">
			<div class="p-6 bg-error/15 rounded-full">
				<AlertCircle class="w-16 h-16 text-error" />
			</div>
			<div class="space-y-2">
				<h1 class="text-2xl font-bold">{$t('common.common.error')}</h1>
				<p class="text-base-content/70">{(error as Error).message}</p>
			</div>
			<button class="btn btn-primary" onclick={() => window.location.reload()}>
				Reload
			</button>
		</div>
	{/snippet}
	</svelte:boundary>

	<!-- Toast notifications -->
	<ToastContainer />
</div>
{:else}
	<div class="flex h-dvh flex-col">
	<DragStrip />
	<div class="flex min-h-0 flex-1 flex-col items-center justify-center gap-4 p-6">
		{#if authError}
			<p class="alert alert-error max-w-md" role="alert">{authError}</p>
			<button class="btn btn-primary" onclick={() => window.location.reload()}>{$t("common.auth.retry")}</button>
		{:else}<span class="loading loading-spinner"></span>{/if}
	</div>
	</div>
{/if}

<style>
	.app-root {
		position: fixed;
		inset: 0;
		display: flex;
		flex-direction: column;
		min-width: 0;
	}
	.app-root.plain {
		background: var(--color-base-200);
	}
	.shell-plain {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		overflow-y: auto;
		overscroll-behavior: none;
	}
	/* Fill whatever the drag strip leaves, instead of a fixed viewport height (avoids a needless scroll) */
	.shell-plain :global(.min-h-screen) {
		flex: 1 0 auto;
		min-height: 0;
	}
	.app-shell {
		flex: 1;
		min-height: 0;
		display: flex;
	}
	.shell-main {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		background: var(--surface-content);
	}
	.shell-scroll {
		flex: 1;
		min-height: 0;
		overflow-x: hidden;
		overflow-y: auto;
		overscroll-behavior: none;
		scroll-behavior: smooth;
	}
	.shell-page {
		width: 100%;
		max-width: 80rem;
		margin-inline: auto;
		padding: 1.25rem 1.5rem 2rem;
	}
	.shell-page :global(.min-h-screen) {
		min-height: 100%;
	}
	@media (max-width: 639px) {
		.shell-page {
			padding: 1rem 0.75rem 1.5rem;
		}
	}
</style>
