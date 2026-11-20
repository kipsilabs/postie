// Push-event payload types for events emitted by postie-web over WebSocket.
// These replace the polled REST endpoints for pause state, running jobs, and
// NNTP pool metrics.

import type { backend, processor } from "$lib/api/models";

export interface ProcessingPauseEvent {
	paused: boolean;
	autoPaused: boolean;
	reason: string;
}

export type RunningJobsEvent = processor.RunningJobDetails[];

export type NntpPoolMetricsEvent = backend.NntpPoolMetrics;

export const EVENT_PROCESSING_PAUSED = "processing:paused";
export const EVENT_PROCESSING_RESUMED = "processing:resumed";
export const EVENT_PROCESSING_AUTO_PAUSED = "processing:auto-paused";
export const EVENT_RUNNING_JOBS_UPDATED = "running-jobs-updated";
export const EVENT_NNTP_POOL_METRICS_UPDATED = "nntp-pool-metrics-updated";
