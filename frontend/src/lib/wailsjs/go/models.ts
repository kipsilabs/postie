export namespace backend {
	
	export class APIQueueUploadRequest {
	    file: string;
	    relative_path: string;
	    priority?: number;
	    delete_after_upload?: boolean;
	
	    static createFrom(source: any = {}) {
	        return new APIQueueUploadRequest(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.file = source["file"];
	        this.relative_path = source["relative_path"];
	        this.priority = source["priority"];
	        this.delete_after_upload = source["delete_after_upload"];
	    }
	}
	export class APIQueueUploadResult {
	    status: string;
	    file: string;
	
	    static createFrom(source: any = {}) {
	        return new APIQueueUploadResult(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.status = source["status"];
	        this.file = source["file"];
	    }
	}
	export class AppStatus {
	    hasConfig: boolean;
	    configPath: string;
	    uploading: boolean;
	    criticalConfigError: boolean;
	    error: string;
	    isFirstStart: boolean;
	    hasServers: boolean;
	    serverCount: number;
	    validServerCount: number;
	    configValid: boolean;
	    needsConfiguration: boolean;
	    version: string;
	
	    static createFrom(source: any = {}) {
	        return new AppStatus(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.hasConfig = source["hasConfig"];
	        this.configPath = source["configPath"];
	        this.uploading = source["uploading"];
	        this.criticalConfigError = source["criticalConfigError"];
	        this.error = source["error"];
	        this.isFirstStart = source["isFirstStart"];
	        this.hasServers = source["hasServers"];
	        this.serverCount = source["serverCount"];
	        this.validServerCount = source["validServerCount"];
	        this.configValid = source["configValid"];
	        this.needsConfiguration = source["needsConfiguration"];
	        this.version = source["version"];
	    }
	}
	export class NntpProviderMetrics {
	    name: string;
	    host: string;
	    activeConnections: number;
	    maxConnections: number;
	    availableSlots: number;
	    totalErrors: number;
	    avgSpeed: number;
	    speedEwma: number;
	    bytesConsumed: number;
	    missing: number;
	    pingRTT: string;
	    ttfb: string;
	    inflight: number;
	    quotaBytes: number;
	    quotaUsed: number;
	    quotaResetAt: string;
	    quotaExceeded: boolean;
	
	    static createFrom(source: any = {}) {
	        return new NntpProviderMetrics(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.name = source["name"];
	        this.host = source["host"];
	        this.activeConnections = source["activeConnections"];
	        this.maxConnections = source["maxConnections"];
	        this.availableSlots = source["availableSlots"];
	        this.totalErrors = source["totalErrors"];
	        this.avgSpeed = source["avgSpeed"];
	        this.speedEwma = source["speedEwma"];
	        this.bytesConsumed = source["bytesConsumed"];
	        this.missing = source["missing"];
	        this.pingRTT = source["pingRTT"];
	        this.ttfb = source["ttfb"];
	        this.inflight = source["inflight"];
	        this.quotaBytes = source["quotaBytes"];
	        this.quotaUsed = source["quotaUsed"];
	        this.quotaResetAt = source["quotaResetAt"];
	        this.quotaExceeded = source["quotaExceeded"];
	    }
	}
	export class NntpPoolMetrics {
	    timestamp: string;
	    activeConnections: number;
	    totalErrors: number;
	    avgSpeed: number;
	    bytesConsumed: number;
	    elapsed: string;
	    providerErrors: Record<string, number>;
	    providers: NntpProviderMetrics[];
	
	    static createFrom(source: any = {}) {
	        return new NntpPoolMetrics(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.timestamp = source["timestamp"];
	        this.activeConnections = source["activeConnections"];
	        this.totalErrors = source["totalErrors"];
	        this.avgSpeed = source["avgSpeed"];
	        this.bytesConsumed = source["bytesConsumed"];
	        this.elapsed = source["elapsed"];
	        this.providerErrors = source["providerErrors"];
	        this.providers = this.convertValues(source["providers"], NntpProviderMetrics);
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	
	export class QueueItem {
	    id: string;
	    path: string;
	    fileName: string;
	    size: number;
	    status: string;
	    retryCount: number;
	    priority: number;
	    errorMessage?: string;
	    // Go type: time
	    createdAt: any;
	    // Go type: time
	    updatedAt: any;
	    // Go type: time
	    completedAt?: any;
	    nzbPath?: string;
	    verificationStatus?: string;
	    verifiedArticles?: number;
	    totalArticles?: number;
	
	    static createFrom(source: any = {}) {
	        return new QueueItem(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.id = source["id"];
	        this.path = source["path"];
	        this.fileName = source["fileName"];
	        this.size = source["size"];
	        this.status = source["status"];
	        this.retryCount = source["retryCount"];
	        this.priority = source["priority"];
	        this.errorMessage = source["errorMessage"];
	        this.createdAt = this.convertValues(source["createdAt"], null);
	        this.updatedAt = this.convertValues(source["updatedAt"], null);
	        this.completedAt = this.convertValues(source["completedAt"], null);
	        this.nzbPath = source["nzbPath"];
	        this.verificationStatus = source["verificationStatus"];
	        this.verifiedArticles = source["verifiedArticles"];
	        this.totalArticles = source["totalArticles"];
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	export class PaginatedQueueResult {
	    items: QueueItem[];
	    totalItems: number;
	    totalPages: number;
	    currentPage: number;
	    itemsPerPage: number;
	    hasNext: boolean;
	    hasPrev: boolean;
	
	    static createFrom(source: any = {}) {
	        return new PaginatedQueueResult(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.items = this.convertValues(source["items"], QueueItem);
	        this.totalItems = source["totalItems"];
	        this.totalPages = source["totalPages"];
	        this.currentPage = source["currentPage"];
	        this.itemsPerPage = source["itemsPerPage"];
	        this.hasNext = source["hasNext"];
	        this.hasPrev = source["hasPrev"];
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	export class PaginationParams {
	    page: number;
	    limit: number;
	    sortBy: string;
	    order: string;
	    status: string;
	
	    static createFrom(source: any = {}) {
	        return new PaginationParams(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.page = source["page"];
	        this.limit = source["limit"];
	        this.sortBy = source["sortBy"];
	        this.order = source["order"];
	        this.status = source["status"];
	    }
	}
	export class ProcessorStatus {
	    hasProcessor: boolean;
	    runningJobs: number;
	    runningJobIDs: string[];
	
	    static createFrom(source: any = {}) {
	        return new ProcessorStatus(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.hasProcessor = source["hasProcessor"];
	        this.runningJobs = source["runningJobs"];
	        this.runningJobIDs = source["runningJobIDs"];
	    }
	}
	
	export class QueueStats {
	    total: number;
	    pending: number;
	    running: number;
	    complete: number;
	    error: number;
	    pendingVerification: number;
	    verificationFailed: number;
	
	    static createFrom(source: any = {}) {
	        return new QueueStats(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.total = source["total"];
	        this.pending = source["pending"];
	        this.running = source["running"];
	        this.complete = source["complete"];
	        this.error = source["error"];
	        this.pendingVerification = source["pendingVerification"];
	        this.verificationFailed = source["verificationFailed"];
	    }
	}
	export class ServerData {
	    name: string;
	    host: string;
	    port: number;
	    username: string;
	    password: string;
	    ssl: boolean;
	    maxConnections: number;
	    role: string;
	
	    static createFrom(source: any = {}) {
	        return new ServerData(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.name = source["name"];
	        this.host = source["host"];
	        this.port = source["port"];
	        this.username = source["username"];
	        this.password = source["password"];
	        this.ssl = source["ssl"];
	        this.maxConnections = source["maxConnections"];
	        this.role = source["role"];
	    }
	}
	export class SetupWizardData {
	    servers: ServerData[];
	    outputDirectory: string;
	    watchDirectory: string;
	
	    static createFrom(source: any = {}) {
	        return new SetupWizardData(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.servers = this.convertValues(source["servers"], ServerData);
	        this.outputDirectory = source["outputDirectory"];
	        this.watchDirectory = source["watchDirectory"];
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	export class TransferRuntimeMetrics {
	    uploadActiveWorkers: number;
	    uploadQueuedWorkers: number;
	    uploadWorkerCount: number;
	    uploadReservedBytes: number;
	    uploadBudgetBytes: number;
	    par2ActiveJobs: number;
	    par2QueuedJobs: number;
	    par2Capacity: number;
	
	    static createFrom(source: any = {}) {
	        return new TransferRuntimeMetrics(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.uploadActiveWorkers = source["uploadActiveWorkers"];
	        this.uploadQueuedWorkers = source["uploadQueuedWorkers"];
	        this.uploadWorkerCount = source["uploadWorkerCount"];
	        this.uploadReservedBytes = source["uploadReservedBytes"];
	        this.uploadBudgetBytes = source["uploadBudgetBytes"];
	        this.par2ActiveJobs = source["par2ActiveJobs"];
	        this.par2QueuedJobs = source["par2QueuedJobs"];
	        this.par2Capacity = source["par2Capacity"];
	    }
	}
	export class ValidationResult {
	    valid: boolean;
	    error: string;
	
	    static createFrom(source: any = {}) {
	        return new ValidationResult(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.valid = source["valid"];
	        this.error = source["error"];
	    }
	}

}

export namespace config {
	
	export class APIConfig {
	    enabled: boolean;
	
	    static createFrom(source: any = {}) {
	        return new APIConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	    }
	}
	export class ArrInstance {
	    id: string;
	    name: string;
	    type: string;
	    url: string;
	    api_key: string;
	    enabled: boolean;
	    webhook_id: number;
	    delete_after_upload: boolean;
	
	    static createFrom(source: any = {}) {
	        return new ArrInstance(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.id = source["id"];
	        this.name = source["name"];
	        this.type = source["type"];
	        this.url = source["url"];
	        this.api_key = source["api_key"];
	        this.enabled = source["enabled"];
	        this.webhook_id = source["webhook_id"];
	        this.delete_after_upload = source["delete_after_upload"];
	    }
	}
	export class ArrConfig {
	    instances: ArrInstance[];
	
	    static createFrom(source: any = {}) {
	        return new ArrConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.instances = this.convertValues(source["instances"], ArrInstance);
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	
	export class PostUploadScriptConfig {
	    enabled: boolean;
	    command: string;
	    timeout: string;
	    max_retries: number;
	    retry_delay: string;
	    max_backoff: string;
	    max_retry_duration: string;
	    retry_check_interval: string;
	
	    static createFrom(source: any = {}) {
	        return new PostUploadScriptConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	        this.command = source["command"];
	        this.timeout = source["timeout"];
	        this.max_retries = source["max_retries"];
	        this.retry_delay = source["retry_delay"];
	        this.max_backoff = source["max_backoff"];
	        this.max_retry_duration = source["max_retry_duration"];
	        this.retry_check_interval = source["retry_check_interval"];
	    }
	}
	export class QueueConfig {
	    max_concurrent_uploads: number;
	    min_size_to_start: number;
	
	    static createFrom(source: any = {}) {
	        return new QueueConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.max_concurrent_uploads = source["max_concurrent_uploads"];
	        this.min_size_to_start = source["min_size_to_start"];
	    }
	}
	export class DatabaseConfig {
	    database_type: string;
	    database_path: string;
	
	    static createFrom(source: any = {}) {
	        return new DatabaseConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.database_type = source["database_type"];
	        this.database_path = source["database_path"];
	    }
	}
	export class NzbCompressionConfig {
	    enabled: boolean;
	    type: string;
	    level: number;
	
	    static createFrom(source: any = {}) {
	        return new NzbCompressionConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	        this.type = source["type"];
	        this.level = source["level"];
	    }
	}
	export class ScheduleConfig {
	    start_time: string;
	    end_time: string;
	
	    static createFrom(source: any = {}) {
	        return new ScheduleConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.start_time = source["start_time"];
	        this.end_time = source["end_time"];
	    }
	}
	export class WatcherConfig {
	    name: string;
	    enabled: boolean;
	    watch_directory: string;
	    size_threshold: number;
	    schedule: ScheduleConfig;
	    ignore_patterns: string[];
	    min_file_size: number;
	    check_interval: string;
	    delete_original_file: boolean;
	    single_nzb_per_folder: boolean;
	    follow_symlinks: boolean;
	    min_file_age: string;
	    min_file_age_to_delete: string;
	
	    static createFrom(source: any = {}) {
	        return new WatcherConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.name = source["name"];
	        this.enabled = source["enabled"];
	        this.watch_directory = source["watch_directory"];
	        this.size_threshold = source["size_threshold"];
	        this.schedule = this.convertValues(source["schedule"], ScheduleConfig);
	        this.ignore_patterns = source["ignore_patterns"];
	        this.min_file_size = source["min_file_size"];
	        this.check_interval = source["check_interval"];
	        this.delete_original_file = source["delete_original_file"];
	        this.single_nzb_per_folder = source["single_nzb_per_folder"];
	        this.follow_symlinks = source["follow_symlinks"];
	        this.min_file_age = source["min_file_age"];
	        this.min_file_age_to_delete = source["min_file_age_to_delete"];
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	export class Par2Config {
	    enabled?: boolean;
	    redundancy: string;
	    temp_dir: string;
	    maintain_par2_files?: boolean;
	    skip_if_par2_exists?: boolean;
	    parpar_binary_path: string;
	    parpar_extra_args: string[];
	    num_goroutines: number;
	    memory_limit: number;
	    slice_size: number;
	    max_concurrent_jobs: number;
	    gf16_method: string;

	    static createFrom(source: any = {}) {
	        return new Par2Config(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	        this.redundancy = source["redundancy"];
	        this.temp_dir = source["temp_dir"];
	        this.maintain_par2_files = source["maintain_par2_files"];
	        this.skip_if_par2_exists = source["skip_if_par2_exists"];
	        this.parpar_binary_path = source["parpar_binary_path"];
	        this.parpar_extra_args = source["parpar_extra_args"];
	        this.num_goroutines = source["num_goroutines"];
	        this.memory_limit = source["memory_limit"];
	        this.slice_size = source["slice_size"];
	        this.max_concurrent_jobs = source["max_concurrent_jobs"];
	        this.gf16_method = source["gf16_method"];
	    }
	}
	export class PostCheck {
	    enabled?: boolean;
	    delay: string;
	    max_reposts: number;
	    deferred_check_delay: string;
	    deferred_max_retries: number;
	    deferred_max_backoff: string;
	    deferred_check_interval: string;
	    deferred_batch_size: number;
	    stat_batch_size: number;
	    max_concurrent_checks: number;
	
	    static createFrom(source: any = {}) {
	        return new PostCheck(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.enabled = source["enabled"];
	        this.delay = source["delay"];
	        this.max_reposts = source["max_reposts"];
	        this.deferred_check_delay = source["deferred_check_delay"];
	        this.deferred_max_retries = source["deferred_max_retries"];
	        this.deferred_max_backoff = source["deferred_max_backoff"];
	        this.deferred_check_interval = source["deferred_check_interval"];
	        this.deferred_batch_size = source["deferred_batch_size"];
	        this.stat_batch_size = source["stat_batch_size"];
	        this.max_concurrent_checks = source["max_concurrent_checks"];
	    }
	}
	export class CustomHeader {
	    name: string;
	    value: string;
	
	    static createFrom(source: any = {}) {
	        return new CustomHeader(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.name = source["name"];
	        this.value = source["value"];
	    }
	}
	export class PostHeaders {
	    add_nxg_header: boolean;
	    default_from: string;
	    custom_headers: CustomHeader[];
	
	    static createFrom(source: any = {}) {
	        return new PostHeaders(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.add_nxg_header = source["add_nxg_header"];
	        this.default_from = source["default_from"];
	        this.custom_headers = this.convertValues(source["custom_headers"], CustomHeader);
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	export class NewsgroupConfig {
	    name: string;
	    enabled?: boolean;
	
	    static createFrom(source: any = {}) {
	        return new NewsgroupConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.name = source["name"];
	        this.enabled = source["enabled"];
	    }
	}
	export class PostingConfig {
	    wait_for_par2?: boolean;
	    max_retries: number;
	    retry_delay: string;
	    article_size_in_bytes: number;
	    groups: NewsgroupConfig[];
	    throttle_rate: number;
	    message_id_format: string;
	    post_headers: PostHeaders;
	    obfuscation_policy: string;
	    par2_obfuscation_policy: string;
	    group_policy: string;
	    upload_buffer_memory_limit: number;
	
	    static createFrom(source: any = {}) {
	        return new PostingConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.wait_for_par2 = source["wait_for_par2"];
	        this.max_retries = source["max_retries"];
	        this.retry_delay = source["retry_delay"];
	        this.article_size_in_bytes = source["article_size_in_bytes"];
	        this.groups = this.convertValues(source["groups"], NewsgroupConfig);
	        this.throttle_rate = source["throttle_rate"];
	        this.message_id_format = source["message_id_format"];
	        this.post_headers = this.convertValues(source["post_headers"], PostHeaders);
	        this.obfuscation_policy = source["obfuscation_policy"];
	        this.par2_obfuscation_policy = source["par2_obfuscation_policy"];
	        this.group_policy = source["group_policy"];
	        this.upload_buffer_memory_limit = source["upload_buffer_memory_limit"];
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	export class ConnectionPoolConfig {
	    min_connections: number;
	    health_check_interval: string;
	
	    static createFrom(source: any = {}) {
	        return new ConnectionPoolConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.min_connections = source["min_connections"];
	        this.health_check_interval = source["health_check_interval"];
	    }
	}
	export class ServerConfig {
	    name?: string;
	    host: string;
	    port: number;
	    username: string;
	    password: string;
	    ssl: boolean;
	    max_connections: number;
	    max_connection_idle_time_in_seconds: number;
	    max_connection_ttl_in_seconds: number;
	    insecure_ssl: boolean;
	    enabled?: boolean;
	    role: string;
	    check_only?: boolean;
	    inflight: number;
	    proxy_url?: string;
	
	    static createFrom(source: any = {}) {
	        return new ServerConfig(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.name = source["name"];
	        this.host = source["host"];
	        this.port = source["port"];
	        this.username = source["username"];
	        this.password = source["password"];
	        this.ssl = source["ssl"];
	        this.max_connections = source["max_connections"];
	        this.max_connection_idle_time_in_seconds = source["max_connection_idle_time_in_seconds"];
	        this.max_connection_ttl_in_seconds = source["max_connection_ttl_in_seconds"];
	        this.insecure_ssl = source["insecure_ssl"];
	        this.enabled = source["enabled"];
	        this.role = source["role"];
	        this.check_only = source["check_only"];
	        this.inflight = source["inflight"];
	        this.proxy_url = source["proxy_url"];
	    }
	}
	export class ConfigData {
	    version: number;
	    servers: ServerConfig[];
	    connection_pool: ConnectionPoolConfig;
	    posting: PostingConfig;
	    post_check: PostCheck;
	    par2: Par2Config;
	    watcher?: WatcherConfig;
	    watchers: WatcherConfig[];
	    nzb_compression: NzbCompressionConfig;
	    database: DatabaseConfig;
	    queue: QueueConfig;
	    api: APIConfig;
	    output_dir: string;
	    maintain_original_extension?: boolean;
	    post_upload_script: PostUploadScriptConfig;
	    arr?: ArrConfig;
	
	    static createFrom(source: any = {}) {
	        return new ConfigData(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.version = source["version"];
	        this.servers = this.convertValues(source["servers"], ServerConfig);
	        this.connection_pool = this.convertValues(source["connection_pool"], ConnectionPoolConfig);
	        this.posting = this.convertValues(source["posting"], PostingConfig);
	        this.post_check = this.convertValues(source["post_check"], PostCheck);
	        this.par2 = this.convertValues(source["par2"], Par2Config);
	        this.watcher = this.convertValues(source["watcher"], WatcherConfig);
	        this.watchers = this.convertValues(source["watchers"], WatcherConfig);
	        this.nzb_compression = this.convertValues(source["nzb_compression"], NzbCompressionConfig);
	        this.database = this.convertValues(source["database"], DatabaseConfig);
	        this.queue = this.convertValues(source["queue"], QueueConfig);
	        this.api = this.convertValues(source["api"], APIConfig);
	        this.output_dir = source["output_dir"];
	        this.maintain_original_extension = source["maintain_original_extension"];
	        this.post_upload_script = this.convertValues(source["post_upload_script"], PostUploadScriptConfig);
	        this.arr = this.convertValues(source["arr"], ArrConfig);
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	
	
	
	
	
	
	
	
	
	
	
	
	

}

export namespace processor {
	
	export class RunningJobDetails {
	    id: string;
	    path: string;
	    fileName: string;
	    size: number;
	    progress: progress.ProgressState[];
	
	    static createFrom(source: any = {}) {
	        return new RunningJobDetails(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.id = source["id"];
	        this.path = source["path"];
	        this.fileName = source["fileName"];
	        this.size = source["size"];
	        this.progress = this.convertValues(source["progress"], progress.ProgressState);
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}
	export class RunningJobItem {
	    id: string;
	
	    static createFrom(source: any = {}) {
	        return new RunningJobItem(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.id = source["id"];
	    }
	}

}

export namespace progress {
	
	export class ProgressState {
	    Max: number;
	    CurrentNum: number;
	    CurrentPercent: number;
	    CurrentBytes: number;
	    SecondsSince: number;
	    SecondsLeft: number;
	    KBsPerSecond: number;
	    Description: string;
	    Type: string;
	    IsStarted: boolean;
	    IsWaiting: boolean;
	    WaitSecondsRemaining: number;
	    IsPaused: boolean;
	
	    static createFrom(source: any = {}) {
	        return new ProgressState(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.Max = source["Max"];
	        this.CurrentNum = source["CurrentNum"];
	        this.CurrentPercent = source["CurrentPercent"];
	        this.CurrentBytes = source["CurrentBytes"];
	        this.SecondsSince = source["SecondsSince"];
	        this.SecondsLeft = source["SecondsLeft"];
	        this.KBsPerSecond = source["KBsPerSecond"];
	        this.Description = source["Description"];
	        this.Type = source["Type"];
	        this.IsStarted = source["IsStarted"];
	        this.IsWaiting = source["IsWaiting"];
	        this.WaitSecondsRemaining = source["WaitSecondsRemaining"];
	        this.IsPaused = source["IsPaused"];
	    }
	}

}

export namespace watcher {
	
	export class WatcherScheduleInfo {
	    start_time: string;
	    end_time: string;
	
	    static createFrom(source: any = {}) {
	        return new WatcherScheduleInfo(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.start_time = source["start_time"];
	        this.end_time = source["end_time"];
	    }
	}
	export class WatcherStatusInfo {
	    name: string;
	    enabled: boolean;
	    initialized: boolean;
	    watch_directory: string;
	    check_interval: string;
	    next_run?: string;
	    is_within_schedule: boolean;
	    schedule?: WatcherScheduleInfo;
	    error?: string;
	
	    static createFrom(source: any = {}) {
	        return new WatcherStatusInfo(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.name = source["name"];
	        this.enabled = source["enabled"];
	        this.initialized = source["initialized"];
	        this.watch_directory = source["watch_directory"];
	        this.check_interval = source["check_interval"];
	        this.next_run = source["next_run"];
	        this.is_within_schedule = source["is_within_schedule"];
	        this.schedule = this.convertValues(source["schedule"], WatcherScheduleInfo);
	        this.error = source["error"];
	    }
	
		convertValues(a: any, classs: any, asMap: boolean = false): any {
		    if (!a) {
		        return a;
		    }
		    if (a.slice && a.map) {
		        return (a as any[]).map(elem => this.convertValues(elem, classs));
		    } else if ("object" === typeof a) {
		        if (asMap) {
		            for (const key of Object.keys(a)) {
		                a[key] = new classs(a[key]);
		            }
		            return a;
		        }
		        return new classs(a);
		    }
		    return a;
		}
	}

}

