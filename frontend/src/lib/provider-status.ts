export interface ProviderSnapshot {
	activeConnections: number;
	totalErrors: number;
}

export type ProviderState = "connected" | "idle" | "failed";

// nntppool only exposes a lifetime error counter, so a provider that retried a
// few hundred articles over a multi-terabyte run would read "Failed" forever
// once its connections went idle. Only a counter that is still climbing while
// nothing is connected means the provider is actually unhealthy right now.
export function classifyProvider(
	current: ProviderSnapshot,
	previous?: ProviderSnapshot,
): ProviderState {
	if (current.activeConnections > 0) return "connected";
	if (previous && current.totalErrors > previous.totalErrors) return "failed";
	return "idle";
}
