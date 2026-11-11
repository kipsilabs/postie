<script lang="ts">
import logo from "$lib/assets/images/logo.png";
import { availableLocales, locale, setStoredLocale, t } from "$lib/i18n";
import { Globe, LogOut, X } from "lucide-svelte";
import { onMount, tick } from "svelte";
import { NAV_ITEMS } from "./navItems";

type Connection = "connected" | "reconnecting" | "disconnected";

interface Props {
	activeRoute: string | null;
	configBlocked: boolean;
	connection: Connection;
	version?: string;
	webAuthenticated: boolean;
	drawerOpen: boolean;
	onLogout: () => void;
	onClose: () => void;
}

let { activeRoute, configBlocked, connection, version, webAuthenticated, drawerOpen, onLogout, onClose }: Props =
	$props();

let activeIndex = $derived(NAV_ITEMS.findIndex((item) => item.id === activeRoute));
let connectionLabel = $derived($t(`common.connection.${connection}`));

// Viewport modes mirrored from the CSS breakpoints below.
let isRail = $state(false);
let isDrawer = $state(false);
let closeButton = $state<HTMLButtonElement>();
let opener: HTMLElement | null = null;
let wasOpen = false;

onMount(() => {
	const railQuery = window.matchMedia("(min-width: 640px) and (max-width: 879px)");
	const drawerQuery = window.matchMedia("(max-width: 639px)");
	const sync = () => {
		isRail = railQuery.matches;
		isDrawer = drawerQuery.matches;
	};
	sync();
	railQuery.addEventListener("change", sync);
	drawerQuery.addEventListener("change", sync);
	return () => {
		railQuery.removeEventListener("change", sync);
		drawerQuery.removeEventListener("change", sync);
	};
});

// Drawer focus handling: move focus in on open, back to the opener on close.
$effect(() => {
	const open = drawerOpen && isDrawer;
	if (open && !wasOpen) {
		opener = document.activeElement as HTMLElement | null;
		tick().then(() => closeButton?.focus());
	} else if (!open && wasOpen) {
		const target = opener;
		opener = null;
		if (target?.isConnected) target.focus();
	}
	wasOpen = open;
});

function handleNavClick(event: MouseEvent, disabled: boolean) {
	if (disabled) {
		event.preventDefault();
		return;
	}
	onClose();
}
</script>

{#if drawerOpen}
	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div class="scrim animate-fade-in" onclick={onClose}></div>
{/if}

<aside
	class="sidebar"
	class:open={drawerOpen}
	role={isDrawer ? "dialog" : undefined}
	aria-modal={isDrawer && drawerOpen ? "true" : undefined}
	aria-label={isDrawer ? $t("common.nav.primary") : undefined}
>
	<div class="drag-spacer" aria-hidden="true"></div>
	<div class="sidebar-strip">
		<img src={logo} alt="" class="logo" />
		<span class="brand">Postie</span>
		<button
			bind:this={closeButton}
			class="btn btn-ghost btn-xs btn-square close-btn"
			aria-label={$t("common.nav.closeMenu")}
			title={$t("common.nav.closeMenu")}
			onclick={onClose}
		>
			<X class="h-4 w-4" />
		</button>
	</div>

	<nav class="nav" aria-label={$t("common.nav.primary")}>
		<div class="nav-list">
			{#if activeIndex >= 0}
				<span class="nav-indicator" style="--idx: {activeIndex}" aria-hidden="true"></span>
			{/if}
			{#each NAV_ITEMS as item (item.id)}
				{@const disabled = item.requiresConfig && configBlocked}
				{@const Icon = item.icon}
				<a
					href={item.href}
					class="nav-item"
					class:active={item.id === activeRoute}
					class:disabled
					title={$t(item.labelKey)}
					aria-current={item.id === activeRoute ? "page" : undefined}
					aria-disabled={disabled ? "true" : undefined}
					tabindex={disabled ? -1 : undefined}
					onclick={(e) => handleNavClick(e, disabled)}
				>
					<Icon class="nav-icon" />
					<span class="nav-label">{$t(item.labelKey)}</span>
				</a>
			{/each}
		</div>
	</nav>

	<div class="sidebar-footer">
		<div class="footer-row">
			<div class="status" title={connectionLabel}>
				<span
					class="dot {connection === 'connected' ? 'bg-success' : connection === 'reconnecting' ? 'bg-warning animate-pulse-soft' : 'bg-error animate-pulse-soft'}"
				></span>
				<span class="footer-text">{connectionLabel}</span>
			</div>
			<div class="dropdown dropdown-end {isRail ? 'dropdown-right' : 'dropdown-top'}">
				<div tabindex="0" role="button" class="btn btn-ghost btn-xs btn-square" title={$t("common.nav.language")} aria-label={$t("common.nav.language")}>
					<Globe class="h-4 w-4" />
				</div>
				<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
				<ul tabindex="0" class="dropdown-content menu z-50 w-40 bg-base-100 p-1">
					{#each availableLocales as lang}
						<li>
							<button
								class:menu-active={$locale === lang.code}
								onclick={() => {
									setStoredLocale(lang.code);
									locale.set(lang.code);
								}}
							>
								{lang.flag} {lang.name}
							</button>
						</li>
					{/each}
				</ul>
			</div>
		</div>
		{#if webAuthenticated}
			<button class="btn btn-ghost btn-sm signout" data-testid="sign-out" title={$t("common.auth.signOut")} onclick={onLogout}>
				<LogOut class="h-4 w-4" />
				<span class="footer-text">{$t("common.auth.signOut")}</span>
			</button>
		{/if}
		{#if version}
			<span class="version footer-text">{version}</span>
		{/if}
	</div>
</aside>

<style>
	.sidebar {
		flex: none;
		width: var(--sidebar-width);
		display: flex;
		flex-direction: column;
		background: var(--sidebar-bg);
		border-right: 1px solid var(--sidebar-border);
		min-height: 0;
		z-index: 40;
	}
	/* macOS desktop: reserves the traffic-light row and drags the window. */
	.drag-spacer {
		flex: none;
		height: var(--titlebar-inset);
		--wails-draggable: drag;
	}
	.sidebar-strip {
		flex: none;
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.75rem 1rem 0.25rem;
	}
	.logo {
		width: 1.75rem;
		height: 1.75rem;
		object-fit: contain;
	}
	.brand {
		font-weight: 600;
		font-size: 1.0625rem;
	}
	.nav {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 0.5rem 0.625rem;
	}
	.nav-list {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: var(--nav-item-gap);
	}
	.nav-indicator {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
		height: var(--nav-item-height);
		border-radius: var(--radius-field);
		background: var(--nav-active-bg);
		transform: translate3d(0, calc(var(--idx) * (var(--nav-item-height) + var(--nav-item-gap))), 0);
		transition: transform var(--motion-base) var(--ease-emphasized);
		will-change: transform;
		pointer-events: none;
	}
	.nav-indicator::before {
		content: "";
		position: absolute;
		left: 0;
		top: 25%;
		bottom: 25%;
		width: 3px;
		border-radius: 3px;
		background: var(--color-primary);
		opacity: var(--nav-pill);
	}
	.nav-item {
		position: relative;
		display: flex;
		align-items: center;
		gap: 0.625rem;
		height: var(--nav-item-height);
		padding: 0 0.625rem;
		border-radius: var(--radius-field);
		font-size: 0.9375rem;
		font-weight: 500;
		color: color-mix(in oklab, var(--color-base-content) 82%, transparent);
		text-decoration: none;
		transition: background-color var(--motion-fast) var(--ease-standard), color var(--motion-fast) var(--ease-standard);
		--wails-draggable: no-drag;
	}
	.nav-item:hover:not(.active):not(.disabled) {
		background: color-mix(in oklab, var(--color-base-content) 6%, transparent);
	}
	.nav-item.active {
		color: var(--color-base-content);
	}
	.nav-item.disabled {
		opacity: 0.4;
		pointer-events: none;
	}
	.nav-item :global(.nav-icon) {
		width: 1.125rem;
		height: 1.125rem;
		flex: none;
		transition: color var(--motion-fast) var(--ease-standard);
	}
	.nav-item.active :global(.nav-icon) {
		color: var(--color-primary);
	}
	.sidebar-footer {
		flex: none;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		padding: 0.5rem 0.625rem 0.75rem;
		border-top: 1px solid var(--sidebar-border);
	}
	.footer-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		padding-inline: 0.25rem;
	}
	.status {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		min-width: 0;
		font-size: 0.8125rem;
		color: color-mix(in oklab, var(--color-base-content) 70%, transparent);
	}
	.dot {
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 9999px;
		flex: none;
	}
	.signout {
		justify-content: flex-start;
	}
	.version {
		padding-inline: 0.25rem;
		font-size: 0.75rem;
		color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
	}
	.scrim {
		display: none;
	}
	.close-btn {
		display: none;
		margin-inline-start: auto;
	}

	/* Icon rail on narrow windows */
	@media (max-width: 879px) {
		.sidebar {
			width: var(--sidebar-rail-width);
		}
		.brand,
		.nav-label,
		.footer-text {
			display: none;
		}
		.sidebar-strip {
			justify-content: center;
			padding-inline: 0;
		}
		.nav {
			padding-inline: 0.5rem;
		}
		.nav-item {
			justify-content: center;
			padding: 0;
		}
		.footer-row {
			flex-direction: column;
			padding-inline: 0;
		}
		.signout {
			justify-content: center;
		}
	}

	/* Overlay drawer on phones / tiny web viewports */
	@media (max-width: 639px) {
		.sidebar {
			position: fixed;
			inset: 0 auto 0 0;
			width: min(var(--sidebar-width), 82vw);
			background: var(--color-base-200);
			box-shadow: var(--ui-shadow-lg);
			transform: translate3d(-100%, 0, 0);
			visibility: hidden;
			transition:
				transform var(--motion-slow) var(--ease-emphasized),
				visibility var(--motion-slow);
		}
		.sidebar.open {
			transform: translate3d(0, 0, 0);
			visibility: visible;
		}
		.brand,
		.nav-label,
		.footer-text {
			display: revert;
		}
		.sidebar-strip {
			justify-content: flex-start;
			padding: 0.75rem 1rem 0.25rem;
		}
		.nav-item {
			justify-content: flex-start;
			padding: 0 0.625rem;
		}
		.footer-row {
			flex-direction: row;
		}
		.signout {
			justify-content: flex-start;
		}
		.close-btn {
			display: inline-flex;
		}
		.scrim {
			display: block;
			position: fixed;
			inset: 0;
			z-index: 39;
			background: color-mix(in oklab, black 40%, transparent);
		}
	}
</style>
