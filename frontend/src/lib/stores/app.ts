import type { backend, processor } from "$lib/api/models";
import { derived, writable } from "svelte/store";

// Create app status store
export const appStatus = writable<backend.AppStatus>({
	needsConfiguration: false,
	criticalConfigError: false,
	configPath: "",
	configValid: false,
	error: "",
	hasConfig: false,
	hasServers: false,
	isFirstStart: false,
	serverCount: 0,
	uploading: false,
	validServerCount: 0,
});

export const runningJobs = writable<processor.RunningJobDetails[]>([]);
// Create upload state store
export const isUploading = derived(runningJobs, (jobs) => {
	return jobs?.length > 0;
});

// Create settings store for save functionality
export const settingsSaveFunction = writable<(() => Promise<void>) | null>(
	null,
);

// Create advanced mode store for settings UI
export const advancedMode = writable<boolean>(false);
