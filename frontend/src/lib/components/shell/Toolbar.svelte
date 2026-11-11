<script lang="ts">
import { t } from "$lib/i18n";
import { toolbar } from "$lib/stores/toolbar";
import { Menu } from "lucide-svelte";
import { NAV_ITEMS } from "./navItems";

interface Props {
	activeRoute: string | null;
	onMenu: () => void;
}

let { activeRoute, onMenu }: Props = $props();

let routeTitle = $derived.by(() => {
	const item = NAV_ITEMS.find((i) => i.id === activeRoute);
	return item ? $t(item.labelKey) : "Postie";
});
let title = $derived($toolbar.title ?? routeTitle);
</script>

<header class="toolbar">
	<button class="btn btn-ghost btn-sm btn-square menu-btn" onclick={onMenu} aria-label={$t("common.nav.menu")}>
		<Menu class="h-5 w-5" />
	</button>
	<h1 class="title">{title}</h1>
	<div class="actions">
		{#if $toolbar.actions}{@render $toolbar.actions()}{/if}
	</div>
</header>

<style>
	.toolbar {
		flex: none;
		display: flex;
		align-items: center;
		gap: 0.5rem;
		height: var(--toolbar-height);
		padding-inline: 1.25rem;
		background: var(--surface-content);
		border-bottom: 1px solid var(--ui-hairline);
		--wails-draggable: drag;
	}
	:global(html[data-platform="mac"][data-desktop="true"]) .toolbar {
		height: max(var(--toolbar-height), var(--titlebar-inset));
	}
	.toolbar :global(:is(button, a, input, select, [role="button"], .dropdown)) {
		--wails-draggable: no-drag;
	}
	.title {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 1rem;
		font-weight: 600;
	}
	.actions {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}
	.menu-btn {
		display: none;
	}
	@media (max-width: 639px) {
		.menu-btn {
			display: inline-flex;
		}
		.toolbar {
			padding-inline: 0.75rem;
		}
	}
</style>
