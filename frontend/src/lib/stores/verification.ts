import apiClient from "$lib/api/client";
import type { backend } from "$lib/api/models";
import type { Handoff } from "$lib/verification-handoff";
import { readable, writable } from "svelte/store";

// Verification has no push events: poll while anything on screen subscribes (one poll shared by
// the progress and verification sections).
const POLL_MS = 3000;

let refreshNow: (() => void) | undefined;

/** Polls right away (a job just ended) instead of waiting for the next tick. */
export function refreshVerificationActivity(): void {
	refreshNow?.();
}

export const verificationActivity = readable<backend.VerificationActivity[]>([], (set) => {
	let stopped = false;
	let inFlight = false;
	async function refresh() {
		if (inFlight || stopped) return;
		inFlight = true;
		try {
			const next = await apiClient.getVerificationActivity();
			if (!stopped) set(next ?? []);
		} catch (error) {
			console.error("Failed to fetch verification progress:", error);
		} finally {
			inFlight = false;
		}
	}
	refresh();
	refreshNow = refresh;
	const timer = setInterval(refresh, POLL_MS);
	return () => {
		stopped = true;
		refreshNow = undefined;
		clearInterval(timer);
	};
});

/** Finished uploads whose verification is shown on their progress card (keyed by item id). */
export const verificationHandoffs = writable<Map<string, Handoff>>(new Map());
