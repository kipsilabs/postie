import { browser } from "$app/environment";
import { readable, writable } from "svelte/store";

/** User-facing appearance choice. */
export type Appearance = "system" | "light" | "dark";
/** Resolved colour scheme actually applied. */
export type ResolvedScheme = "light" | "dark";

export const APPEARANCES: readonly Appearance[] = ["system", "light", "dark"];
export const THEME_STORAGE_KEY = "theme";
const THEME_NAMES: Record<ResolvedScheme, string> = {
	light: "postie-light",
	dark: "postie-dark",
};

function isAppearance(value: unknown): value is Appearance {
	return value === "system" || value === "light" || value === "dark";
}

function readStored(): Appearance {
	if (!browser) return "system";
	try {
		const saved = localStorage.getItem(THEME_STORAGE_KEY);
		if (isAppearance(saved)) return saved;
		// Legacy daisyUI theme name (cupcake, dracula, ...) migrates to System.
		if (saved !== null) localStorage.setItem(THEME_STORAGE_KEY, "system");
	} catch {
		// storage unavailable
	}
	return "system";
}

function systemScheme(): ResolvedScheme {
	if (!browser) return "light";
	return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function resolve(appearance: Appearance): ResolvedScheme {
	return appearance === "system" ? systemScheme() : appearance;
}

function applyScheme(scheme: ResolvedScheme) {
	if (!browser) return;
	document.documentElement.setAttribute("data-theme", THEME_NAMES[scheme]);
}

function createAppearanceStore() {
	const appearance = writable<Appearance>(readStored());
	const resolved = writable<ResolvedScheme>(resolve(readStored()));
	let current: Appearance = readStored();
	let transitionTimer: number | undefined;

	if (browser) {
		applyScheme(resolve(current));
		window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
			if (current !== "system") return;
			const scheme = systemScheme();
			applyScheme(scheme);
			resolved.set(scheme);
		});
	}

	function set(next: Appearance) {
		if (!isAppearance(next)) return;
		current = next;
		appearance.set(next);
		if (!browser) return;
		try {
			localStorage.setItem(THEME_STORAGE_KEY, next);
		} catch {
			// storage unavailable
		}
		const scheme = resolve(next);
		const root = document.documentElement;
		root.classList.add("theme-transition");
		applyScheme(scheme);
		resolved.set(scheme);
		if (transitionTimer !== undefined) window.clearTimeout(transitionTimer);
		const base = Number.parseFloat(getComputedStyle(root).getPropertyValue("--motion-base")) || 200;
		transitionTimer = window.setTimeout(() => {
			root.classList.remove("theme-transition");
			transitionTimer = undefined;
		}, base + 50);
	}

	return {
		/** Current choice: "system" | "light" | "dark". */
		subscribe: appearance.subscribe,
		set,
		/** Resolved light/dark scheme currently applied. */
		resolved: readable<ResolvedScheme>("light", (setValue) => resolved.subscribe(setValue)),
	};
}

export const appearance = createAppearanceStore();
