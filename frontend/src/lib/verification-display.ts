// How a verification activity item reads on screen, shared by the progress and verification sections.
import type { backend } from "$lib/api/models";

type Phase = backend.VerificationActivity["phase"];
type Translate = (key: string, options?: { values?: Record<string, string | number> }) => string;

export function phaseDot(phase: Phase): string {
	switch (phase) {
		case "checking":
			return "bg-primary animate-pulse-soft";
		case "repairing":
			return "bg-warning animate-pulse-soft";
		case "deferred":
			return "bg-warning";
		default:
			return "bg-info";
	}
}

function secondsUntil(at: string | null | undefined, now: number): number {
	if (!at) return 0;
	return Math.max(0, Math.ceil((Date.parse(at) - now) / 1000));
}

export function phaseDetail(item: backend.VerificationActivity, t: Translate, now: number): string {
	switch (item.phase) {
		case "checking":
			return t("dashboard.verification.detail.checking", {
				values: { verified: item.verifiedArticles, total: item.totalArticles },
			});
		case "repairing":
			return t("dashboard.verification.detail.repairing", {
				values: { missing: item.missingArticles },
			});
		case "waiting":
			return t("dashboard.verification.detail.waiting", {
				values: { seconds: secondsUntil(item.nextCheckAt, now) },
			});
		case "deferred":
			return t("dashboard.verification.detail.deferred");
		default:
			return t("dashboard.verification.detail.queued");
	}
}
