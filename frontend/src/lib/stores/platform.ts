import { browser } from "$app/environment";
import { readable } from "svelte/store";

export type Platform = "mac" | "windows" | "linux";

export interface PlatformInfo {
	os: Platform;
	/** true when running inside the Wails desktop shell, false for the web build. */
	desktop: boolean;
}

function normalizeOS(value: string | null | undefined): Platform {
	if (value === "mac" || value === "darwin") return "mac";
	if (value === "windows") return "windows";
	return "linux";
}

function readFromDocument(): PlatformInfo {
	if (!browser) return { os: "linux", desktop: false };
	const root = document.documentElement;
	return {
		os: normalizeOS(root.getAttribute("data-platform")),
		desktop: root.getAttribute("data-desktop") === "true",
	};
}

let current: PlatformInfo = readFromDocument();
let setStore: ((value: PlatformInfo) => void) | undefined;

/** Reactive platform info. Initial value comes from the inline script in app.html. */
export const platform = readable<PlatformInfo>(current, (set) => {
	setStore = set;
	set(current);
	return () => {
		setStore = undefined;
	};
});

/** Synchronous snapshot (non-reactive). */
export function getPlatform(): PlatformInfo {
	return current;
}

function publish(next: PlatformInfo) {
	current = next;
	if (browser) {
		const root = document.documentElement;
		root.setAttribute("data-platform", next.os);
		root.setAttribute("data-desktop", String(next.desktop));
	}
	setStore?.(next);
}

/**
 * Confirms the platform with the Wails runtime in desktop mode. Safe to call
 * in the browser: it leaves the web-mode guess from app.html untouched.
 */
export async function initPlatform(): Promise<PlatformInfo> {
	if (!browser) return current;
	const win = window as unknown as { go?: unknown; wails?: unknown };
	if (!win.go && !win.wails) return current;
	try {
		const { Environment } = await import("$lib/wailsjs/runtime/runtime");
		const env = await Environment();
		publish({ os: normalizeOS(env.platform), desktop: true });
	} catch {
		publish({ ...current, desktop: true });
	}
	return current;
}
