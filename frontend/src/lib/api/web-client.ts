// Web API client for browser environment (replaces Wails bindings)

import type { backend, config, processor, watcher } from "$lib/wailsjs/go/models";
// Using backend types for pagination

const API_BASE = "/api";

export interface ArrInstance {
	id: string;
	name: string;
	type: "radarr" | "sonarr" | "lidarr" | "readarr";
	url: string;
	api_key: string;
	enabled: boolean;
	webhook_id: number;
	delete_after_upload: boolean;
}

export class WebClient {
	private ws: WebSocket | null = null;
	private wsListeners: Map<string, (data: unknown) => void> = new Map();
	private reconnectListeners = new Set<() => void>();

	constructor() {
		this.initWebSocket();
	}

	// Wait for WebSocket to be connected
	async waitForConnection(): Promise<void> {
		return new Promise((resolve, reject) => {
			if (this.ws?.readyState === WebSocket.OPEN) {
				resolve();
				return;
			}

			const checkConnection = () => {
				if (this.ws?.readyState === WebSocket.OPEN) {
					resolve();
				} else if (
					this.ws?.readyState === WebSocket.CLOSED ||
					this.ws?.readyState === WebSocket.CLOSING
				) {
					reject(new Error("WebSocket connection failed"));
				} else {
					setTimeout(checkConnection, 100);
				}
			};

			checkConnection();
		});
	}

	private initWebSocket() {
		const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
		const wsUrl = `${protocol}//${window.location.host}/api/ws`;
		console.log("WebSocket URL:", wsUrl);
		console.log("Attempting WebSocket connection...");

		this.ws = new WebSocket(wsUrl);

		this.ws.onopen = (event) => {
			console.log("WebSocket connected successfully", event);
			for (const listener of this.reconnectListeners) {
				listener();
			}
		};

		this.ws.onmessage = (event) => {
			try {
				const message = JSON.parse(event.data);

				// Check if this is a structured message with type and data
				if (message.type && message.data !== undefined) {
					// Dispatch to specific event listener
					const listener = this.wsListeners.get(message.type);
					if (listener) {
						listener(message.data);
					}
				} else {
					// Fallback: emit to all listeners for unstructured messages
					for (const listener of this.wsListeners.values()) {
						listener(message);
					}
				}
			} catch (error) {
				console.error("Error parsing WebSocket message:", error);
			}
		};

		this.ws.onclose = (event) => {
			console.log("WebSocket disconnected:", event.code, event.reason);
			console.log("Attempting to reconnect in 3 seconds...");
			setTimeout(() => this.initWebSocket(), 3000);
		};

		this.ws.onerror = (error) => {
			console.error("WebSocket error:", error);
			console.log("WebSocket state:", this.ws?.readyState);
		};
	}

	// Add event listener for real-time updates
	async on(event: string, callback: (data: unknown) => void): Promise<void> {
		// Wait for WebSocket connection before registering listener
		try {
			await this.waitForConnection();
		} catch (error) {
			console.error(
				"Failed to connect WebSocket before registering listener:",
				error,
			);
		}

		this.wsListeners.set(event, callback);
	}

	// Remove event listener
	off(event: string) {
		this.wsListeners.delete(event);
	}

	isConnected(): boolean {
		return this.ws?.readyState === WebSocket.OPEN;
	}

	// Fires on every successful (re)connection so event-driven views can re-sync.
	onReconnect(callback: () => void): () => void {
		this.reconnectListeners.add(callback);
		return () => {
			this.reconnectListeners.delete(callback);
		};
	}

	// Requests that hang (e.g. the backend blocked on the database during a
	// heavy upload batch) must not pile up and exhaust the browser's per-host
	// connection pool — that is what froze the dashboard until a manual
	// refresh. Abort instead, so the next poll gets a fresh attempt.
	private static readonly REQUEST_TIMEOUT_MS = 15000;

	async get<T>(endpoint: string): Promise<T> {
		const response = await fetch(`${API_BASE}${endpoint}`, {
			signal: AbortSignal.timeout(WebClient.REQUEST_TIMEOUT_MS),
		});
		if (!response.ok) {
			throw new Error(`HTTP error! status: ${response.status}`);
		}
		return response.json();
	}

	async post<T>(endpoint: string, data?: unknown): Promise<T> {
		const response = await fetch(`${API_BASE}${endpoint}`, {
			method: "POST",
			headers: {
				"Content-Type": "application/json",
			},
			body: data ? JSON.stringify(data) : undefined,
			signal: AbortSignal.timeout(WebClient.REQUEST_TIMEOUT_MS),
		});

		if (!response.ok) {
			const errText = await response.text();
			throw new Error(errText.trim() || `HTTP error! status: ${response.status}`);
		}

		// Handle empty responses
		const text = await response.text();
		return text ? JSON.parse(text) : ({} as T);
	}

	async delete<T>(endpoint: string): Promise<T> {
		const response = await fetch(`${API_BASE}${endpoint}`, {
			method: "DELETE",
			signal: AbortSignal.timeout(WebClient.REQUEST_TIMEOUT_MS),
		});

		if (!response.ok) {
			throw new Error(`HTTP error! status: ${response.status}`);
		}

		const text = await response.text();
		return text ? JSON.parse(text) : ({} as T);
	}

	// App methods
	async getStatus(): Promise<backend.AppStatus> {
		return this.get<backend.AppStatus>("/status");
	}

	async getConfig(): Promise<config.ConfigData> {
		return this.get<config.ConfigData>("/config");
	}

	async saveConfig(config: config.ConfigData): Promise<void> {
		return this.post<void>("/config", config);
	}

	async getQueueItems(params: backend.PaginationParams): Promise<backend.PaginatedQueueResult> {
		const queryParams = new URLSearchParams({
			page: params.page.toString(),
			limit: params.limit.toString(),
			sortBy: params.sortBy,
			order: params.order,
		});
		if (params.status) {
			queryParams.set("status", params.status);
		}
		return this.get<backend.PaginatedQueueResult>(`/queue?${queryParams}`);
	}

	async getQueueStats(): Promise<backend.QueueStats> {
		return this.get<backend.QueueStats>("/queue/stats");
	}

	async retryJob(id: string): Promise<void> {
		return this.post<void>(`/queue/${id}/retry`);
	}

	async cancelJob(id: string): Promise<void> {
		return this.delete<void>(`/queue/${id}/cancel`);
	}

	// Processor methods
	async getProcessorStatus(): Promise<backend.ProcessorStatus> {
		return this.get<backend.ProcessorStatus>("/processor/status");
	}

	async getWatcherStatus(): Promise<watcher.WatcherStatusInfo[]> {
		return this.get<watcher.WatcherStatusInfo[]>("/watcher/status");
	}

	async triggerScan(): Promise<void> {
		return this.post<void>("/watcher/scan");
	}

	async getApiKey(): Promise<string> {
		const res = await this.get<{ key: string }>("/api-key");
		return res.key;
	}

	async regenerateApiKey(): Promise<string> {
		const res = await this.post<{ key: string }>("/api-key/regenerate");
		return res.key;
	}

	async pauseProcessing(): Promise<void> {
		return this.post<void>("/processor/pause");
	}

	async resumeProcessing(): Promise<void> {
		return this.post<void>("/processor/resume");
	}

	async isProcessingPaused(): Promise<boolean> {
		const response = await this.get<{ paused: boolean }>("/processor/paused");
		return response.paused;
	}

	async isProcessingAutoPaused(): Promise<boolean> {
		const response = await this.get<{ autoPaused: boolean }>("/processor/auto-paused");
		return response.autoPaused;
	}

	async getAutoPauseReason(): Promise<string> {
		const response = await this.get<{ reason: string }>("/processor/auto-pause-reason");
		return response.reason;
	}

	async getRunningJobs(): Promise<processor.RunningJobItem[]> {
		return this.get<processor.RunningJobItem[]>("/running-jobs");
	}

	async getRunningJobDetails(): Promise<Promise<processor.RunningJobDetails[]>> {
		return this.get<Promise<processor.RunningJobDetails[]>>("/running-job-details");
	}

	// Logs
	async getLogs(limit?: number, offset?: number): Promise<string> {
		const params = new URLSearchParams();
		if (limit) params.append("limit", limit.toString());
		if (offset) params.append("offset", offset.toString());

		const endpoint = `/logs${params.toString() ? `?${params.toString()}` : ""}`;
		const response = await fetch(`${API_BASE}${endpoint}`);

		if (!response.ok) {
			throw new Error(`HTTP error! status: ${response.status}`);
		}

		return response.text();
	}

	// Queue management
	async removeFromQueue(id: string): Promise<void> {
		return this.delete<void>(`/queue/${id}`);
	}

	async setQueueItemPriority(id: string, priority: number): Promise<void> {
		return this.post<void>(`/queue/${id}/priority`, { priority });
	}

	async clearQueue(): Promise<void> {
		return this.delete<void>("/queue");
	}

	async addFilesToQueue(): Promise<void> {
		// This triggers a file picker dialog in the browser
		// The actual file selection is handled by the frontend
		return this.post<void>("/queue/add-files");
	}

	// Upload management
	async uploadFiles(
		files: FileList,
		onProgress?: (progress: number) => void,
		setRequest?: (xhr: XMLHttpRequest) => void,
	): Promise<void> {
		const formData = new FormData();
		for (let i = 0; i < files.length; i++) {
			formData.append("files", files[i]);
		}

		return new Promise((resolve, reject) => {
			const xhr = new XMLHttpRequest();

			// Store the request reference for cancellation
			if (setRequest) {
				setRequest(xhr);
			}

			// Track upload progress
			if (onProgress) {
				xhr.upload.addEventListener("progress", (event) => {
					if (event.lengthComputable) {
						const progress = (event.loaded / event.total) * 100;
						onProgress(progress);
					}
				});
			}

			xhr.addEventListener("load", () => {
				if (xhr.status >= 200 && xhr.status < 300) {
					resolve();
				} else {
					reject(new Error(`HTTP error! status: ${xhr.status}`));
				}
			});

			xhr.addEventListener("error", () => {
				reject(new Error("Upload failed"));
			});

			xhr.addEventListener("abort", () => {
				reject(new Error("Upload cancelled"));
			});

			xhr.open("POST", `${API_BASE}/upload`);
			xhr.send(formData);
		});
	}

	async cancelUpload(): Promise<void> {
		return this.post<void>("/upload/cancel");
	}

	// Upload folder files preserving directory structure (for webkitdirectory uploads)
	async uploadFolderFiles(
		files: FileList,
		onProgress?: (progress: number) => void,
		setRequest?: (xhr: XMLHttpRequest) => void,
	): Promise<void> {
		const formData = new FormData();

		// Extract root folder name from first file's webkitRelativePath
		const firstFile = files[0] as File & { webkitRelativePath?: string };
		const rootFolder = firstFile.webkitRelativePath?.split("/")[0] || "folder";
		formData.append("folderName", rootFolder);

		// Append files with their relative paths
		for (let i = 0; i < files.length; i++) {
			const file = files[i] as File & { webkitRelativePath?: string };
			const relativePath = file.webkitRelativePath || file.name;
			formData.append("files", file);
			formData.append("paths", relativePath);
		}

		return new Promise((resolve, reject) => {
			const xhr = new XMLHttpRequest();

			// Store the request reference for cancellation
			if (setRequest) {
				setRequest(xhr);
			}

			// Track upload progress
			if (onProgress) {
				xhr.upload.addEventListener("progress", (event) => {
					if (event.lengthComputable) {
						const progress = (event.loaded / event.total) * 100;
						onProgress(progress);
					}
				});
			}

			xhr.addEventListener("load", () => {
				if (xhr.status >= 200 && xhr.status < 300) {
					resolve();
				} else {
					reject(new Error(`HTTP error! status: ${xhr.status}`));
				}
			});

			xhr.addEventListener("error", () => {
				reject(new Error("Upload failed"));
			});

			xhr.addEventListener("abort", () => {
				reject(new Error("Upload cancelled"));
			});

			xhr.open("POST", `${API_BASE}/upload-folder`);
			xhr.send(formData);
		});
	}

	// NZB operations
	async downloadNZB(id: string): Promise<void> {
		const response = await fetch(`${API_BASE}/nzb/${id}/download`);

		if (!response.ok) {
			throw new Error(`HTTP error! status: ${response.status}`);
		}

		// Create a blob from the response and trigger download
		const blob = await response.blob();
		const url = window.URL.createObjectURL(blob);
		const filename = response.headers.get("Content-Disposition")?.match(/filename="(.+?)"/)?.[1] || `${id}.nzb`;
		const a = document.createElement("a");
		a.href = url;
		a.download = filename;
		document.body.appendChild(a);
		a.click();
		window.URL.revokeObjectURL(url);
		document.body.removeChild(a);
	}

	// Setup Wizard
	async validateNNTPServer(
		serverData: backend.ServerData,
	): Promise<backend.ValidationResult> {
		return this.post<backend.ValidationResult>("/validate-server", serverData);
	}

	async testProviderConnectivity(
		serverData: backend.ServerData,
	): Promise<backend.ValidationResult> {
		return this.post<backend.ValidationResult>("/test-provider-connectivity", serverData);
	}

	async setupWizardComplete(
		wizardData: backend.SetupWizardData,
	): Promise<void> {
		return this.post<void>("/setup/complete", wizardData);
	}

	// Pending Config Management
	async hasPendingConfigChanges(): Promise<boolean> {
		return this.get<boolean>("/config/pending/status");
	}

	async getPendingConfigStatus(): Promise<Record<string, unknown>> {
		return this.get<Record<string, unknown>>("/config/pending");
	}

	async applyPendingConfig(): Promise<void> {
		return this.post<void>("/config/pending/apply", {});
	}

	async discardPendingConfig(): Promise<void> {
		return this.post<void>("/config/pending/discard", {});
	}

	async getAppliedConfig(): Promise<config.ConfigData> {
		return this.get<config.ConfigData>("/config/applied");
	}

	// NNTP Pool Metrics
	async getNntpPoolMetrics(): Promise<backend.NntpPoolMetrics> {
		return this.get<backend.NntpPoolMetrics>("/metrics/nntp-pool");
	}

	async getTransferRuntimeMetrics(): Promise<backend.TransferRuntimeMetrics> {
		return this.get<backend.TransferRuntimeMetrics>("/metrics/transfer-runtime");
	}

	// Filesystem operations
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
		const params = new URLSearchParams({ path });
		return this.get(`/filesystem/browse?${params}`);
	}

	async importFiles(filePaths: string[]): Promise<{
		success: boolean;
		importedCount: number;
		message: string;
	}> {
		return this.post("/filesystem/import", { filePaths });
	}

	// Log download
	async downloadLogs(): Promise<void> {
		const response = await fetch(`${API_BASE}/logs/download`);

		if (!response.ok) {
			throw new Error(`HTTP error! status: ${response.status}`);
		}

		// Create a blob from the response and trigger download
		const blob = await response.blob();
		const url = window.URL.createObjectURL(blob);
		const filename =
			response.headers
				.get("Content-Disposition")
				?.match(/filename="(.+?)"/)?.[1] ||
			`postie-${new Date().toISOString().split("T")[0]}.log`;
		const a = document.createElement("a");
		a.href = url;
		a.download = filename;
		document.body.appendChild(a);
		a.click();
		window.URL.revokeObjectURL(url);
		document.body.removeChild(a);
	}

	// ── Arr webhook integration ─────────────────────────────────────────────

	async getArrInstances(): Promise<ArrInstance[]> {
		const res = await fetch(`${API_BASE}/arr/instances`);
		if (!res.ok) throw new Error(await res.text());
		return res.json();
	}

	async addArrInstance(instance: ArrInstance): Promise<ArrInstance> {
		const webhookURL = window.location.origin;
		const res = await fetch(`${API_BASE}/arr/instances`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ instance, webhook_url: webhookURL }),
		});
		if (!res.ok) throw new Error(await res.text());
		return res.json();
	}

	async removeArrInstance(id: string): Promise<void> {
		const res = await fetch(`${API_BASE}/arr/instances/${id}`, { method: "DELETE" });
		if (!res.ok) throw new Error(await res.text());
	}

	async testArrConnection(instance: ArrInstance): Promise<void> {
		const res = await fetch(`${API_BASE}/arr/instances/${instance.id}/test`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(instance),
		});
		if (!res.ok) throw new Error(await res.text());
	}
}

// Singleton instance
let webClientInstance: WebClient | null = null;

export function getWebClient(): WebClient {
	if (!webClientInstance) {
		console.log("Initializing web client");
		webClientInstance = new WebClient();
	}
	return webClientInstance;
}
