import { describe, expect, it } from "bun:test";
import type { backend, processor } from "$lib/api/models";
import {
	HANDOFF_GRACE_MS,
	retrack,
	VERIFIED_LINGER_MS,
	type Handoff,
	startHandoffs,
	track,
	updateHandoffs,
} from "./verification-handoff";

const job = (id: string, pct?: number) =>
	({
		id,
		path: `/d/${id}`,
		fileName: id,
		size: 1,
		progress:
			pct === undefined
				? []
				: [{ Type: "checking", Description: `${id} (verifying)`, CurrentPercent: pct }],
	}) as unknown as processor.RunningJobDetails;

const activity = (id: string, verified: number, total = 100) =>
	({
		id,
		path: `/d/${id}`,
		phase: "checking",
		totalArticles: total,
		verifiedArticles: verified,
		missingArticles: 0,
	}) as backend.VerificationActivity;

const handed = (pct?: number) =>
	startHandoffs(new Map(), new Map([["a", track(job("a", pct))]]), new Set(), 0);

describe("verification hand-off", () => {
	it("keeps the verifying row's label and progress when a job stops running", () => {
		const h = handed(0.6).get("a") as Handoff;
		expect(h.label).toBe("a (verifying)");
		expect(h.held).toBe(0.6);
		expect(startHandoffs(handed(0.6), new Map(), new Set(["a"]), 0).has("a")).toBe(false);
	});

	it("keeps the label and progress when the verifying row goes away before the job ends", () => {
		let t = retrack(new Map(), [job("a", 0.7)]);
		t = retrack(t, [job("a")]); // posting returned, job still finishing
		const h = startHandoffs(new Map(), t, new Set(), 0).get("a") as Handoff;
		expect(h.label).toBe("a (verifying)");
		expect(h.held).toBe(0.7);
	});

	it("ignores checking rows that are not the verifying row", () => {
		const resumed = {
			...job("a"),
			progress: [{ Type: "checking", Description: "a (resume check)", CurrentPercent: 0.9 }],
		} as unknown as processor.RunningJobDetails;
		expect(track(resumed).held).toBe(0);
	});

	it("never moves backwards while the sweep catches up", () => {
		let h = updateHandoffs(handed(0.6), [activity("a", 10)], 1000);
		expect(h.get("a")?.held).toBe(0.6);
		h = updateHandoffs(h, [activity("a", 90)], 2000);
		expect(h.get("a")?.held).toBe(0.9);
	});

	it("shows a verified item at 100% briefly, then drops it", () => {
		let h = updateHandoffs(handed(0.6), [activity("a", 50)], 1000);
		h = updateHandoffs(h, [], 2000);
		expect(h.get("a")?.held).toBe(1);
		expect(updateHandoffs(h, [], 2000 + VERIFIED_LINGER_MS).has("a")).toBe(false);
	});

	it("drops a job that never shows up in the activity after the grace", () => {
		expect(updateHandoffs(handed(), [], HANDOFF_GRACE_MS - 1).has("a")).toBe(true);
		expect(updateHandoffs(handed(), [], HANDOFF_GRACE_MS).has("a")).toBe(false);
	});
});
