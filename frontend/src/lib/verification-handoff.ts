// When an upload ends, its card stays in the progress section and its verifying row carries on,
// fed by the verification activity, instead of a second bar appearing in the verification panel.
// A running job's id is its completed item's id, which is the activity id.
import type { backend, processor } from "$lib/api/models";

/** How long a finished job waits to show up in the activity before its card is dropped (the
 * activity is polled, and a failed or cancelled job never shows up). */
export const HANDOFF_GRACE_MS = 6_000;
/** How long a verified card lingers at 100% before it goes away. */
export const VERIFIED_LINGER_MS = 3_000;

export interface Tracked {
	id: string;
	path: string;
	fileName: string;
	size: number;
	/** The verifying row's description, kept so the row reads the same after the hand-off. */
	label: string;
	/** Fraction confirmed so far; the shown progress never goes below it. */
	held: number;
}

export interface Handoff extends Tracked {
	since: number;
	item?: backend.VerificationActivity;
	/** Set once the item left the activity after being seen in it: verified. */
	doneAt?: number;
}

const clamp = (n: number) => Math.min(1, Math.max(0, Number.isFinite(n) ? n : 0));

export function activityRatio(item: backend.VerificationActivity): number {
	return item.totalArticles > 0 ? clamp(item.verifiedArticles / item.totalArticles) : 0;
}

/** The row the early checker advances: "<file> (verifying)" or "Verifying N files" (not the
 * resume check or the legacy check, which are "checking" rows too). */
function isVerifyingRow(p: processor.RunningJobDetails["progress"][number]): boolean {
	const d = p.Description ?? "";
	return p.Type === "checking" && (d.endsWith("(verifying)") || d.startsWith("Verifying "));
}

/** What a running job hands off if it ends now. */
export function track(job: processor.RunningJobDetails): Tracked {
	const row = job.progress?.find(isVerifyingRow);
	return {
		id: job.id,
		path: job.path,
		fileName: job.fileName,
		size: job.size,
		label: row?.Description || `${job.fileName} (verifying)`,
		held: row ? clamp(row.CurrentPercent) : 0,
	};
}

/** Re-snapshots the running jobs, keeping what each already showed: the verifying row goes away
 * when posting returns, while the job is still finishing (NZB, transfer completion), and a
 * snapshot from then must not hand off 0%. */
export function retrack(
	prev: Map<string, Tracked>,
	jobs: processor.RunningJobDetails[],
): Map<string, Tracked> {
	return new Map(
		jobs.map((job) => {
			const now = track(job);
			const old = prev.get(job.id);
			if (!old) return [job.id, now];
			const hasRow = job.progress?.some(isVerifyingRow) ?? false;
			return [
				job.id,
				{ ...now, label: hasRow ? now.label : old.label, held: Math.max(old.held, now.held) },
			];
		}),
	);
}

/** Starts a hand-off for every tracked job that is no longer running. */
export function startHandoffs(
	handoffs: Map<string, Handoff>,
	before: Map<string, Tracked>,
	runningIds: Set<string>,
	now: number,
): Map<string, Handoff> {
	const next = new Map(handoffs);
	for (const [id, t] of before) {
		if (!runningIds.has(id) && !next.has(id)) next.set(id, { ...t, since: now });
	}
	// a job that runs again (a retry) is a running card, not a hand-off
	for (const id of runningIds) next.delete(id);
	return next;
}

/** Applies an activity snapshot: progress only moves forward, a hand-off whose item left the
 * activity is verified (shown briefly at 100%), and one never seen in it expires after the grace. */
export function updateHandoffs(
	handoffs: Map<string, Handoff>,
	activity: backend.VerificationActivity[],
	now: number,
): Map<string, Handoff> {
	const byId = new Map(activity.map((a) => [a.id, a]));
	const next = new Map<string, Handoff>();
	for (const [id, h] of handoffs) {
		const item = byId.get(id);
		if (item) {
			next.set(id, { ...h, item, held: Math.max(h.held, activityRatio(item)), doneAt: undefined });
			continue;
		}
		if (h.item || h.doneAt !== undefined) {
			const doneAt = h.doneAt ?? now;
			if (now - doneAt < VERIFIED_LINGER_MS) next.set(id, { ...h, held: 1, doneAt });
			continue;
		}
		if (now - h.since < HANDOFF_GRACE_MS) next.set(id, h);
	}
	return next;
}
