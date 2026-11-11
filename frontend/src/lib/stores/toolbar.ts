import type { Snippet } from "svelte";
import { writable } from "svelte/store";

export interface ToolbarState {
	/** Overrides the default (route based) toolbar title. */
	title?: string;
	/** Optional snippet rendered on the right side of the toolbar. */
	actions?: Snippet;
}

const empty: ToolbarState = {};
const { subscribe, set } = writable<ToolbarState>(empty);

/**
 * Page-level toolbar customisation. In a page:
 *   $effect(() => { toolbar.set({ title: "…", actions: myActions }); return () => toolbar.reset(); });
 */
export const toolbar = {
	subscribe,
	set: (state: ToolbarState) => set(state),
	reset: () => set(empty),
};
