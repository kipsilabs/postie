export interface LiveFallbackOptions {
	intervalMs: number;
	isLive: () => boolean;
	refresh: () => void | Promise<void>;
	onReconnect?: (callback: () => void) => () => void;
}

// Event-driven panels go stale when the push channel silently dies (a half-open
// WebSocket behind Docker/NAT, a proxy that never upgraded). This keeps them
// honest: poll while the channel is down, and re-sync once when it comes back,
// since the broadcaster only pushes running-jobs on change.
export function startLiveFallback(options: LiveFallbackOptions): () => void {
	const { intervalMs, isLive, refresh, onReconnect } = options;
	let stopped = false;

	const safeRefresh = () => {
		if (stopped) return;
		Promise.resolve()
			.then(refresh)
			.catch((error) => console.error("Live fallback refresh failed:", error));
	};

	const timer = setInterval(() => {
		if (!isLive()) safeRefresh();
	}, intervalMs);

	const unsubscribe = onReconnect?.(safeRefresh);

	return () => {
		stopped = true;
		clearInterval(timer);
		unsubscribe?.();
	};
}
