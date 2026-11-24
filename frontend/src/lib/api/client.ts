// Unified API client: talks HTTP/WebSocket to postie-web. The Tauri desktop shell loads the same
// UI from its in-process loopback server and adds native dialogs through `tauriInvoke`.

import { isTauri, tauriInvoke } from "./tauri";
import { browser } from "$app/environment";
import type { backend, config, processor, watcher } from "$lib/api/models";
import type { ArrInstance, WebClient } from "./web-client";
export type { ArrInstance };

function isWebEnvironment(): boolean {
	return browser && typeof fetch !== "undefined";
}

// Lazy import to avoid bundling issues
let webClient: WebClient | null = null;

async function getWebClient() {
	if (!webClient) {
		const { getWebClient: getWebClientFn } = await import("./web-client");
		webClient = getWebClientFn();
	}
	return webClient;
}

// Event handling abstraction
type EventCallback = (data: unknown) => void;

const eventListeners = new Map<string, Set<EventCallback>>();

// Unified API interface
export class UnifiedClient {
	private _isReady = false;
	private _environment: "web" | "unknown" = "unknown";

	async initialize(): Promise<void> {
		if (this._isReady) return;

		if (!browser) {
			this._environment = "unknown";
			return;
		}

		if (isWebEnvironment()) {
			this._environment = "web";
			// Initialize web client
			await getWebClient();
		} else {
			this._environment = "unknown";
			console.warn("Unable to detect environment, some features may not work");
		}

		this._isReady = true;
	}

	get environment(): "web" | "unknown" {
		return this._environment;
	}

	get isReady(): boolean {
		return this._isReady;
	}

	// App Status
	async getAppStatus(): Promise<backend.AppStatus> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getStatus();
		}

		throw new Error("No client available");
	}

	// Configuration
	async getConfig(): Promise<config.ConfigData> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getConfig();
		}

		throw new Error("No client available");
	}

	async saveConfig(config: config.ConfigData): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.saveConfig(config);
		}
	}

	// Queue Management
	async getQueueItems(params: backend.PaginationParams): Promise<backend.PaginatedQueueResult> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getQueueItems(params);
		}

		throw new Error("No client available");
	}

	async getQueueStats(): Promise<backend.QueueStats> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getQueueStats();
		}

		throw new Error("No client available");
	}

	async retryJob(id: string): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.retryJob(id);
		}

		throw new Error("No client available");
	}

	async cancelJob(id: string): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.cancelJob(id);
		}
	}

	async removeFromQueue(id: string): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.removeFromQueue(id);
		}
	}

	async setQueueItemPriority(id: string, priority: number): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.setQueueItemPriority(id, priority);
		}
	}

	// Processing
	async getProcessorStatus(): Promise<backend.ProcessorStatus> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getProcessorStatus();
		}

		throw new Error("No client available");
	}

	async getWatcherStatus(): Promise<watcher.WatcherStatusInfo[]> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getWatcherStatus();
		}

		throw new Error("No client available");
	}

	async triggerScan(): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.triggerScan();
		}

		throw new Error("No client available");
	}

	async getApiKey(): Promise<string> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getApiKey();
		}

		throw new Error("No client available");
	}

	async regenerateApiKey(): Promise<string> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.regenerateApiKey();
		}

		throw new Error("No client available");
	}

	async getVerificationActivity(): Promise<backend.VerificationActivity[]> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getVerificationActivity();
		}

		throw new Error("No client available");
	}

	async getRunningJobDetails(): Promise<processor.RunningJobDetails[]> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getRunningJobDetails();
		}

		throw new Error("No client available");
	}

	async pauseProcessing(): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.pauseProcessing();
		}

		throw new Error("No client available");
	}

	async resumeProcessing(): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.resumeProcessing();
		}

		throw new Error("No client available");
	}

	async isProcessingPaused(): Promise<boolean> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.isProcessingPaused();
		}

		throw new Error("No client available");
	}

	async isProcessingAutoPaused(): Promise<boolean> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.isProcessingAutoPaused();
		}

		throw new Error("No client available");
	}

	async getAutoPauseReason(): Promise<string> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getAutoPauseReason();
		}

		throw new Error("No client available");
	}

	async uploadFiles(): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			// Web upload is handled via file input and drag/drop
			throw new Error("Use uploadFileList for web uploads");
		}
	}

	async uploadFileList(
		files: FileList,
		onProgress?: (progress: number) => void,
		setRequest?: (xhr: XMLHttpRequest) => void,
	): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();

			// Check if this is a folder upload (files have webkitRelativePath)
			const firstFile = files[0] as File & { webkitRelativePath?: string };
			if (firstFile?.webkitRelativePath) {
				// Use folder upload endpoint to preserve directory structure
				return client.uploadFolderFiles(files, onProgress, setRequest);
			}

			// Regular file upload
			return client.uploadFiles(files, onProgress, setRequest);
		}

		throw new Error("No client available");
	}

	// Desktop only: native file picker returning absolute paths (empty when cancelled)
	async selectFiles(): Promise<string[]> {
		await this.initialize();

		if (isTauri()) {
			return tauriInvoke<string[]>("select_files");
		}

		return [];
	}

	// Desktop only: queue files by path; they are read in place, never copied
	async addFilePaths(paths: string[]): Promise<void> {
		await this.initialize();

		if (isTauri()) {
			return tauriInvoke<void>("add_files", { paths });
		}

		throw new Error("Adding files by path is only available in the desktop app");
	}

	// Folder upload
	async selectFolder(): Promise<string | null> {
		await this.initialize();

		if (isTauri()) {
			return (await tauriInvoke<string | null>("select_folder")) || null;
		}

		// In web mode, folder selection is handled via HTML input with webkitdirectory
		return null;
	}

	async uploadFolder(folderPath: string): Promise<void> {
		await this.initialize();

		if (isTauri()) {
			return tauriInvoke<void>("upload_folders", { paths: [folderPath] });
		}

		// In web mode, folder upload is handled via uploadFileList with webkitdirectory files
		throw new Error("Use uploadFileList for web folder uploads");
	}

	async selectFolders(): Promise<string[]> {
		await this.initialize();

		if (isTauri()) {
			return tauriInvoke<string[]>("select_folders");
		}

		// In web mode, folder selection is handled via HTML input with webkitdirectory
		return [];
	}

	async uploadFolders(folderPaths: string[]): Promise<void> {
		await this.initialize();

		if (isTauri()) {
			return tauriInvoke<void>("upload_folders", { paths: folderPaths });
		}

		// In web mode, folder upload is handled via uploadFileList with webkitdirectory files
		throw new Error("Use uploadFileList for web folder uploads");
	}

	// Logs
	async getLogs(): Promise<string> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getLogs();
		}

		throw new Error("No client available");
	}

	async getLogsPaginated(limit: number, offset: number): Promise<string> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getLogs(limit, offset);
		}

		throw new Error("No client available");
	}

	async downloadLogFile(): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.downloadLogs();
		}

		throw new Error("No client available");
	}

	// Event Handling
	async on(event: string, callback: EventCallback): Promise<void> {
		await this.initialize();

		// Store callback for cleanup
		if (!eventListeners.has(event)) {
			eventListeners.set(event, new Set());
		}
		const listeners = eventListeners.get(event);
		if (listeners) {
			listeners.add(callback);
		}

		if (this._environment === "web") {
			const client = await getWebClient();
			client.on(event, callback);
		}
	}

	async off(event: string, callback?: EventCallback): Promise<void> {
		await this.initialize();

		if (callback) {
			eventListeners.get(event)?.delete(callback);
		} else {
			eventListeners.get(event)?.clear();
		}

		if (this._environment === "web") {
			const client = await getWebClient();
			client.off(event);
		}
	}

	// True when push events are flowing: an OPEN WebSocket with recent event traffic.
	isLive(): boolean {
		if (this._environment === "web") return webClient?.isConnected() ?? false;
		return false;
	}

	onReconnect(callback: () => void): () => void {
		if (this._environment !== "web") return () => {};
		let unsubscribe: (() => void) | undefined;
		let cancelled = false;
		getWebClient().then((client) => {
			if (cancelled) return;
			unsubscribe = client.onReconnect(callback);
		});
		return () => {
			cancelled = true;
			unsubscribe?.();
		};
	}

	// Queue Actions
	async clearQueue(): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.clearQueue();
		}

		throw new Error("No client available");
	}

	async addFilesToQueue(): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			// In web mode, we handle file selection via HTML input
			// This method serves as a trigger for the file picker
			const client = await getWebClient();
			return client.addFilesToQueue();
		}

		throw new Error("No client available");
	}

	async cancelUpload(): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.cancelUpload();
		}

		throw new Error("No client available");
	}

	// NZB operations
	async downloadNZB(id: string): Promise<void> {
		await this.initialize();

		if (isTauri()) {
			return tauriInvoke<void>("download_nzb", { id });
		}

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.downloadNZB(id);
		}

		throw new Error("No client available");
	}

	// Navigation (handled by SvelteKit; kept for callers)
	async navigateToSettings(): Promise<void> {
		await this.initialize();

		// In web mode, navigation is handled by SvelteKit
	}

	async navigateToDashboard(): Promise<void> {
		await this.initialize();

		// In web mode, navigation is handled by SvelteKit
	}

	// Directory Selection
	async selectTempDirectory(): Promise<string> {
		await this.initialize();

		if (isTauri()) {
			return (await tauriInvoke<string | null>("select_folder")) ?? "";
		}

		if (this._environment === "web") {
			// In web mode, directory selection would need to be handled differently
			// For now, return empty string to indicate no selection
			return "";
		}

		throw new Error("No client available");
	}

	// Setup Wizard
	async validateNNTPServer(
		serverData: backend.ServerData,
	): Promise<backend.ValidationResult> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.validateNNTPServer(serverData);
		}

		throw new Error("No client available");
	}

	async testProviderConnectivity(
		serverData: backend.ServerData,
	): Promise<backend.ValidationResult> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.testProviderConnectivity(serverData);
		}

		throw new Error("No client available");
	}

	async setupWizardComplete(
		wizardData: backend.SetupWizardData,
	): Promise<void> {
		console.log("Completing setup wizard with data:", wizardData);
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.setupWizardComplete(wizardData);
		}

		throw new Error("No client available");
	}

	async getAppliedConfig(): Promise<config.ConfigData> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getAppliedConfig();
		}

		throw new Error("No client available");
	}

	// Pending Config Management
	async hasPendingConfigChanges(): Promise<boolean> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.hasPendingConfigChanges();
		}

		throw new Error("No client available");
	}

	async getPendingConfigStatus(): Promise<Record<string, unknown>> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getPendingConfigStatus();
		}

		throw new Error("No client available");
	}

	async applyPendingConfig(): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.applyPendingConfig();
		}

		throw new Error("No client available");
	}

	async discardPendingConfig(): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.discardPendingConfig();
		}

		throw new Error("No client available");
	}

	/** True in the Tauri desktop shell (native dialogs available). */
	get isDesktop(): boolean {
		return isTauri();
	}

	async selectWatchDirectory(): Promise<string> {
		await this.initialize();

		if (isTauri()) {
			return (await tauriInvoke<string | null>("select_folder")) ?? "";
		}

		throw new Error("Desktop only");
	}

	async getConfigPath(): Promise<string> {
		await this.initialize();

		if (isTauri()) {
			return tauriInvoke<string>("get_config_path");
		}

		throw new Error("Desktop only");
	}

	async selectConfigDirectory(): Promise<string> {
		await this.initialize();

		if (isTauri()) {
			return tauriInvoke<string>("select_config_directory");
		}

		throw new Error("Desktop only");
	}

	async selectOutputDirectory(): Promise<string> {
		await this.initialize();

		if (isTauri()) {
			return (await tauriInvoke<string | null>("select_folder")) ?? "";
		}

		throw new Error("No client available");
	}

	// NNTP Pool Metrics
	async getNntpPoolMetrics(): Promise<backend.NntpPoolMetrics> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getNntpPoolMetrics();
		}

		throw new Error("No client available");
	}

	async getTransferRuntimeMetrics(): Promise<backend.TransferRuntimeMetrics> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getTransferRuntimeMetrics();
		}

		throw new Error("No client available");
	}

	// Filesystem operations (web-only)
	async browseFilesystem(path: string): Promise<{
		path: string;
		items: Array<{
			name: string;
			path: string;
			isDir: boolean;
			size: number;
			modTime: string;
		}>;
	}> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.browseFilesystem(path);
		}

		throw new Error("No client available");
	}

	async importFiles(filePaths: string[]): Promise<{
		success: boolean;
		importedCount: number;
		message: string;
	}> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.importFiles(filePaths);
		}

		throw new Error("No client available");
	}

	// ── Arr webhook integration (web mode only) ────────────────────────────

	async getArrInstances(): Promise<ArrInstance[]> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.getArrInstances();
		}

		throw new Error("Arr webhook integration is only available in web mode");
	}

	async addArrInstance(instance: ArrInstance): Promise<ArrInstance> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.addArrInstance(instance);
		}

		throw new Error("Arr webhook integration is only available in web mode");
	}

	async removeArrInstance(id: string): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.removeArrInstance(id);
		}

		throw new Error("Arr webhook integration is only available in web mode");
	}

	async testArrConnection(instance: ArrInstance): Promise<void> {
		await this.initialize();

		if (this._environment === "web") {
			const client = await getWebClient();
			return client.testArrConnection(instance);
		}

		throw new Error("Arr webhook integration is only available in web mode");
	}
}

// Export singleton instance
export const apiClient = new UnifiedClient();
export default apiClient;
