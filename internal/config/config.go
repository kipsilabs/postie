package config

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"fmt"
	"log/slog"
	"net"
	"net/mail"
	"os"
	"slices"
	"strings"
	"time"

	"github.com/javi11/nntppool/v4"
	"golang.org/x/net/proxy"
	"gopkg.in/yaml.v3"
)

const (
	defaultRedundancy = "5%" //https://github.com/animetosho/ParPar/blob/6feee4dd94bb18480f0bf08cd9d17ffc7e671b69/help-full.txt#L75
	// CurrentConfigVersion represents the current configuration version
	CurrentConfigVersion = 2
)

// ServerRole defines how a server is used in the pool.
type ServerRole string

const (
	// ServerRoleUpload indicates the server is used for posting articles.
	ServerRoleUpload ServerRole = "upload"
	// ServerRoleVerify indicates the server is used only for article verification (STAT checks).
	ServerRoleVerify ServerRole = "verify"
)

// Duration wraps time.Duration to provide custom JSON and YAML marshalling
type Duration string

// MarshalJSON implements json.Marshaler interface
func (d Duration) MarshalJSON() ([]byte, error) {
	return json.Marshal(string(d))
}

// UnmarshalJSON implements json.Unmarshaler interface
func (d *Duration) UnmarshalJSON(data []byte) error {
	var s string
	if err := json.Unmarshal(data, &s); err != nil {
		return err
	}

	// Allow empty duration strings (treat as zero) so partial configs or missing fields don't hard-fail.
	// Defaults (where applicable) are applied at higher levels (e.g. config load / validation).
	if s == "" {
		*d = Duration("")
		return nil
	}

	duration, err := time.ParseDuration(s)
	if err != nil {
		return err
	}

	*d = Duration(duration.String())
	return nil
}

// MarshalYAML implements yaml.Marshaler interface
func (d Duration) MarshalYAML() (any, error) {
	return string(d), nil
}

// UnmarshalYAML implements yaml.Unmarshaler interface
func (d *Duration) UnmarshalYAML(value *yaml.Node) error {
	var s string
	if err := value.Decode(&s); err != nil {
		return err
	}

	// Allow empty duration strings (treat as zero) so partial configs or missing fields don't hard-fail.
	// Defaults (where applicable) are applied at higher levels (e.g. config load / validation).
	if s == "" {
		// #endregion agent log
		*d = Duration("")
		return nil
	}

	duration, err := time.ParseDuration(s)
	if err != nil {
		return err
	}

	*d = Duration(duration.String())
	return nil
}

// ToDuration converts Duration to time.Duration
func (d Duration) ToDuration() time.Duration {
	duration, _ := time.ParseDuration(string(d))
	return duration
}

type CompressionType string

const (
	// No compression
	CompressionTypeNone CompressionType = "none"
	// Zstandard compression
	CompressionTypeZstd CompressionType = "zstd"
	// Brotli compression
	CompressionTypeBrotli CompressionType = "brotli"
	// ZIP compression
	CompressionTypeZip CompressionType = "zip"
)

type GroupPolicy string

const (
	//    ALL       : everything is posted on ALL the Groups
	GroupPolicyAll GroupPolicy = "all"
	//    EACH_FILE : each File will be posted on a random Group from the list (only with Article's obfuscation)
	GroupPolicyEachFile GroupPolicy = "each_file"
)

type MessageIDFormat string

const (
	// NXG: the Message-ID will be formatted as https://github.com/javi11/nxg
	MessageIDFormatNXG MessageIDFormat = "nxg"
	// Random: the Message-ID will be a random string of 32 characters
	MessageIDFormatRandom MessageIDFormat = "random"
)

type ObfuscationPolicy string

const (
	// Will do the following obfuscation:
	// - Subject: will be obfuscated
	// - Filename: will be obfuscated
	// - Yenc header filename: will be randomized for every article
	// - Date: will be randomized for every article within last 6 hours
	// - NXG-header: will not be added
	// - Poster: will be random for each article
	ObfuscationPolicyFull ObfuscationPolicy = "full"
	// Will do the following obfuscation:
	// - Subject: will be obfuscated
	// - Filename: will be obfuscated
	// - Yenc header filename: will be same one for all articles
	// - Date: will be the real posted date
	// - Poster: will be the same one for all articles
	ObfuscationPolicyPartial ObfuscationPolicy = "partial"
	// Nothing will be obfuscated
	ObfuscationPolicyNone ObfuscationPolicy = "none"
)

type Config interface {
	GetNNTPPool() (*nntppool.Client, error)
	GetPostingConfig() PostingConfig
	GetPostCheckConfig() PostCheck
	GetPar2Config(ctx context.Context) (*Par2Config, error)
	GetWatcherConfig() WatcherConfig
	GetWatcherConfigs() []WatcherConfig
	GetNzbCompressionConfig() NzbCompressionConfig
	GetDatabaseConfig() DatabaseConfig
	GetQueueConfig() QueueConfig
	GetAPIConfig() APIConfig
	GetPostUploadScriptConfig() PostUploadScriptConfig
	GetMaintainOriginalExtension() bool
}

type ConnectionPoolConfig struct {
	MinConnections      int      `yaml:"min_connections" json:"min_connections"`
	HealthCheckInterval Duration `yaml:"health_check_interval" json:"health_check_interval"`
}

// config is the internal implementation of the Config interface
type ConfigData struct {
	Version        int                  `yaml:"version" json:"version"`
	Servers        []ServerConfig       `yaml:"servers" json:"servers"`
	ConnectionPool ConnectionPoolConfig `yaml:"connection_pool" json:"connection_pool"`
	Posting        PostingConfig        `yaml:"posting" json:"posting"`
	// Check uploaded article configuration. used to check if an article was successfully uploaded and propagated.
	PostCheck PostCheck  `yaml:"post_check" json:"post_check"`
	Par2      Par2Config `yaml:"par2" json:"par2"`
	// Watcher is deprecated: use Watchers instead. Retained for backward-compatible YAML parsing.
	Watcher                   WatcherConfig          `yaml:"watcher,omitempty" json:"watcher,omitempty"`
	Watchers                  []WatcherConfig        `yaml:"watchers" json:"watchers"`
	NzbCompression            NzbCompressionConfig   `yaml:"nzb_compression" json:"nzb_compression"`
	Database                  DatabaseConfig         `yaml:"database" json:"database"`
	Queue                     QueueConfig            `yaml:"queue" json:"queue"`
	API                       APIConfig              `yaml:"api" json:"api"`
	OutputDir                 string                 `yaml:"output_dir" json:"output_dir"`
	MaintainOriginalExtension *bool                  `yaml:"maintain_original_extension" json:"maintain_original_extension"`
	PostUploadScript          PostUploadScriptConfig `yaml:"post_upload_script" json:"post_upload_script"`
	Arr                       ArrConfig              `yaml:"arr,omitempty" json:"arr,omitempty"`
}

type Par2Config struct {
	Enabled           *bool    `yaml:"enabled" json:"enabled"`
	Redundancy        string   `yaml:"redundancy" json:"redundancy"`
	TempDir           string   `yaml:"temp_dir" json:"temp_dir"`
	MaintainPar2Files *bool    `yaml:"maintain_par2_files" json:"maintain_par2_files"`
	SkipIfPar2Exists  *bool    `yaml:"skip_if_par2_exists" json:"skip_if_par2_exists"`
	ParparBinaryPath  string   `yaml:"parpar_binary_path" json:"parpar_binary_path"`
	ParparExtraArgs   []string `yaml:"parpar_extra_args" json:"parpar_extra_args"`
	NumGoroutines     int      `yaml:"num_goroutines" json:"num_goroutines"`
	MemoryLimit       int64    `yaml:"memory_limit" json:"memory_limit"`
	SliceSize         int64    `yaml:"slice_size" json:"slice_size"`
	// MaxConcurrentJobs caps how many PAR2 operations the process-wide PAR2
	// scheduler runs at once. Additional work is queued rather than run
	// concurrently, so MemoryLimit applies per active job instead of per queue
	// job. Default value is `1`.
	MaxConcurrentJobs int `yaml:"max_concurrent_jobs" json:"max_concurrent_jobs"`
	// GF16Method pins the PAR2 GF16 SIMD kernel used by the native executor.
	// "auto" lets par2go pick a safe kernel for the CPU; anything else forces
	// that kernel even if the CPU does not support it (PAR2 creation then fails).
	GF16Method string `yaml:"gf16_method" json:"gf16_method"`
}

const Par2GF16MethodAuto = "auto"

// Par2GF16Methods lists the accepted values for Par2Config.GF16Method.
var Par2GF16Methods = []string{
	Par2GF16MethodAuto,
	"lookup",
	"lookup3",
	"shuffle-avx2",
	"shuffle-avx512",
	"shuffle-vbmi",
	"xor-jit-avx2",
	"affine-avx2",
	"affine-avx512",
	"shuffle-neon",
	"clmul-neon",
}

// ServerConfig represents a Usenet server configuration
type ServerConfig struct {
	// Name is an optional custom display label shown in place of the auto-numbered
	// "Provider N" / "Server N" in the UI. Purely cosmetic — never sent to nntppool.
	Name                           string `yaml:"name,omitempty" json:"name,omitempty"`
	Host                           string `yaml:"host" json:"host"`
	Port                           int    `yaml:"port" json:"port"`
	Username                       string `yaml:"username" json:"username"`
	Password                       string `yaml:"password" json:"password"`
	SSL                            bool   `yaml:"ssl" json:"ssl"`
	MaxConnections                 int    `yaml:"max_connections" json:"max_connections"`
	MaxConnectionIdleTimeInSeconds int    `yaml:"max_connection_idle_time_in_seconds" json:"max_connection_idle_time_in_seconds"`
	MaxConnectionTTLInSeconds      int    `yaml:"max_connection_ttl_in_seconds" json:"max_connection_ttl_in_seconds"`
	InsecureSSL                    bool   `yaml:"insecure_ssl" json:"insecure_ssl"`
	Enabled                        *bool  `yaml:"enabled" json:"enabled"`
	// Role defines how this server is used: "upload" for posting, "verify" for STAT checks only.
	// All upload-role servers must share the same provider host.
	Role ServerRole `yaml:"role" json:"role"`
	// CheckOnly is deprecated: use Role instead. Retained for backward-compatible YAML parsing (v1 configs).
	CheckOnly *bool `yaml:"check_only,omitempty" json:"check_only,omitempty"`
	// Inflight sets the number of concurrent requests per connection. 0 defaults to 1 in nntppool v4.
	Inflight int `yaml:"inflight" json:"inflight"`
	// SOCKS5 Proxy URL (optional, format: socks5://username:password@hostname:port)
	ProxyURL string `yaml:"proxy_url,omitempty" json:"proxy_url,omitempty"`
}

type PostHeaders struct {
	// Whether to add the X-NXG header to the uploaded articles (You will still see this header in the generated NZB). Default value is `true`.
	// If obfuscation policy is `FULL` this header will not be added.
	// If message_id_format is not `nxg` this header will not be added.
	AddNXGHeader bool `yaml:"add_nxg_header" json:"add_nxg_header"`
	// The default from header for the uploaded articles. By default a random poster will be used for each article. This will override GenerateFromByArticle
	DefaultFrom string `yaml:"default_from" json:"default_from"`
	// Add custom headers to the uploaded articles. Subject, From, Newsgroups, Message-ID and Date can not be override.
	CustomHeaders []CustomHeader `yaml:"custom_headers" json:"custom_headers"`
}

type CustomHeader struct {
	Name  string `yaml:"name" json:"name"`
	Value string `yaml:"value" json:"value"`
}

type PostCheck struct {
	// If enabled articles will be checked after being posted. Default value is `true`.
	Enabled *bool `yaml:"enabled" json:"enabled"`
	// Delay between retries. Default value is `10s`.
	RetryDelay Duration `yaml:"delay" json:"delay"`
	// The maximum number of re-posts if article check fails. Default value is `1`.
	MaxRePost uint `yaml:"max_reposts" json:"max_reposts"`
	// Initial delay before first deferred recheck. Default value is `30s`.
	// Auto-enabled when PostCheck.Enabled is true.
	DeferredCheckDelay Duration `yaml:"deferred_check_delay" json:"deferred_check_delay"`
	// Maximum number of deferred check retry attempts. Default value is `5`.
	DeferredMaxRetries int `yaml:"deferred_max_retries" json:"deferred_max_retries"`
	// Maximum backoff cap for deferred checks. Default value is `5m`.
	DeferredMaxBackoff Duration `yaml:"deferred_max_backoff" json:"deferred_max_backoff"`
	// Worker poll interval for deferred checks. Default value is `2m`.
	DeferredCheckInterval Duration `yaml:"deferred_check_interval" json:"deferred_check_interval"`
	// Number of articles processed per deferred check cycle. Default value is
	// 10000, sized so a typical complete NZB is swept in a single cycle.
	DeferredBatchSize int `yaml:"deferred_batch_size" json:"deferred_batch_size"`
	// Number of segments checked per batched STAT sweep. Default value is 100.
	StatBatchSize int `yaml:"stat_batch_size" json:"stat_batch_size"`
	// MaxConcurrentChecks caps the number of concurrent STAT verification checks
	// across the whole process. A value of 0 enables automatic sizing: the
	// durable verification service uses a dedicated verification pool capped at
	// 16, or a shared upload pool capped at 2, never exceeding pool capacity.
	// Default value is `0` (auto).
	MaxConcurrentChecks int `yaml:"max_concurrent_checks" json:"max_concurrent_checks"`
}

// NewsgroupConfig represents a single newsgroup configuration
type NewsgroupConfig struct {
	Name    string `yaml:"name" json:"name"`
	Enabled *bool  `yaml:"enabled" json:"enabled"`
}

// PostingConfig represents posting configuration
type PostingConfig struct {
	WaitForPar2        *bool             `yaml:"wait_for_par2" json:"wait_for_par2"`
	MaxRetries         int               `yaml:"max_retries" json:"max_retries"`
	RetryDelay         Duration          `yaml:"retry_delay" json:"retry_delay"`
	ArticleSizeInBytes uint64            `yaml:"article_size_in_bytes" json:"article_size_in_bytes"`
	Groups             []NewsgroupConfig `yaml:"groups" json:"groups"`
	ThrottleRate       int64             `yaml:"throttle_rate" json:"throttle_rate"` // bytes per second
	MessageIDFormat    MessageIDFormat   `yaml:"message_id_format" json:"message_id_format"`
	PostHeaders        PostHeaders       `yaml:"post_headers" json:"post_headers"`
	// If true the uploaded subject and filename will be obfuscated. Default value is `true`.
	ObfuscationPolicy     ObfuscationPolicy `yaml:"obfuscation_policy" json:"obfuscation_policy"`
	Par2ObfuscationPolicy ObfuscationPolicy `yaml:"par2_obfuscation_policy" json:"par2_obfuscation_policy"`
	//  If you give several Groups you've 3 policy when posting
	GroupPolicy GroupPolicy `yaml:"group_policy" json:"group_policy"`
	// UploadBufferMemoryLimit caps the total bytes the process-wide upload engine
	// may reserve for in-flight raw + encoded article buffers, independent of
	// queue concurrency. A value of 0 enables automatic sizing based on connection
	// capacity and article size. Default value is `0` (auto).
	UploadBufferMemoryLimit int64 `yaml:"upload_buffer_memory_limit" json:"upload_buffer_memory_limit"`
}

type WatcherConfig struct {
	Name               string         `yaml:"name" json:"name"`
	Enabled            bool           `yaml:"enabled" json:"enabled"`
	WatchDirectory     string         `yaml:"watch_directory" json:"watch_directory"`
	SizeThreshold      int64          `yaml:"size_threshold" json:"size_threshold"`
	Schedule           ScheduleConfig `yaml:"schedule" json:"schedule"`
	IgnorePatterns     []string       `yaml:"ignore_patterns" json:"ignore_patterns"`
	MinFileSize        int64          `yaml:"min_file_size" json:"min_file_size"`
	CheckInterval      Duration       `yaml:"check_interval" json:"check_interval"`
	DeleteOriginalFile bool           `yaml:"delete_original_file" json:"delete_original_file"`
	// If true, creates one NZB per folder instead of one NZB per file in watch mode. Default value is `false`.
	SingleNzbPerFolder bool `yaml:"single_nzb_per_folder" json:"single_nzb_per_folder"`
	// FollowSymlinks controls whether symbolic links are followed during directory scanning.
	// If false (default), symlinks are skipped to avoid double-counting files and including
	// files outside the watch directory. Set to true to process symlinks as regular files.
	FollowSymlinks bool `yaml:"follow_symlinks" json:"follow_symlinks"`
	// MinFileAge is the minimum time since last modification before a file is eligible for upload.
	// This prevents uploading files that are still being written to (e.g. slow copies, torrents).
	// Default: 60s
	MinFileAge Duration `yaml:"min_file_age" json:"min_file_age"`
	// MinFileAgeToDelete is the minimum time to wait after a successful upload before
	// deleting the original file. Useful when a downstream tool (e.g. AltMount) needs
	// time to import the generated NZB before the source file disappears.
	// Requires DeleteOriginalFile=true. Default: 0 (delete immediately).
	MinFileAgeToDelete Duration `yaml:"min_file_age_to_delete" json:"min_file_age_to_delete"`
}

type ScheduleConfig struct {
	StartTime string `yaml:"start_time" json:"start_time"`
	EndTime   string `yaml:"end_time" json:"end_time"`
}

// NzbCompressionConfig represents the NZB compression configuration
type NzbCompressionConfig struct {
	// Whether to enable compression. Default is false.
	Enabled bool `yaml:"enabled" json:"enabled"`
	// Compression type to use. Default is "none".
	Type CompressionType `yaml:"type" json:"type"`
	// Compression level to use. Default depends on the compression type.
	Level int `yaml:"level" json:"level"`
}

// DatabaseConfig represents the database configuration
type DatabaseConfig struct {
	// Database type to use. Supported: "sqlite", "postgres", "mysql"
	DatabaseType string `yaml:"database_type" json:"database_type"`
	// Database connection string or file path
	DatabasePath string `yaml:"database_path" json:"database_path"`
}

// QueueConfig represents the upload queue configuration
type QueueConfig struct {
	// Maximum concurrent uploads from queue
	MaxConcurrentUploads int `yaml:"max_concurrent_uploads" json:"max_concurrent_uploads"`
	// MinSizeToStart holds back queue processing until the cumulative size of
	// pending file jobs (in bytes) reaches this floor. Useful when uploads are
	// pushed via the HTTP API and you want to batch them into a single drain.
	// 0 disables gating (default).
	MinSizeToStart int64 `yaml:"min_size_to_start" json:"min_size_to_start"`
}

// APIConfig represents the external HTTP API configuration. The API exposes a
// gated endpoint (POST /api/v1/queue/upload) for pushing files to the upload
// queue. The API key itself is stored in SQLite, not here.
type APIConfig struct {
	// Enabled controls whether the gated endpoint is mounted and serves traffic.
	// When false, requests to /api/v1/queue/upload return 403.
	Enabled bool `yaml:"enabled" json:"enabled"`
}

// PostUploadScriptConfig represents the post upload script configuration
type PostUploadScriptConfig struct {
	// Whether to enable the post upload script execution. Default value is `false`.
	Enabled bool `yaml:"enabled" json:"enabled"`
	// Command to execute after NZB generation. Use {nzb_path} placeholder for the NZB file path
	Command string `yaml:"command" json:"command"`
	// Timeout for script execution. Default value is `30s`.
	Timeout Duration `yaml:"timeout" json:"timeout"`
	// Maximum number of retry attempts for failed script executions.
	// Set to 0 for unlimited retries (will use MaxRetryDuration as the limit).
	// Default value is `3`.
	MaxRetries int `yaml:"max_retries" json:"max_retries"`
	// Base delay for retry attempts with exponential backoff. Default value is `30s`.
	RetryDelay Duration `yaml:"retry_delay" json:"retry_delay"`
	// Maximum backoff duration. Caps the exponential backoff to prevent very long waits.
	// Default value is `1h`.
	MaxBackoff Duration `yaml:"max_backoff" json:"max_backoff"`
	// Maximum duration to keep retrying after the first failure.
	// After this duration, the script is marked as permanently failed.
	// Default value is `24h`.
	MaxRetryDuration Duration `yaml:"max_retry_duration" json:"max_retry_duration"`
	// How often to check for pending retries. Default value is `1m`.
	RetryCheckInterval Duration `yaml:"retry_check_interval" json:"retry_check_interval"`
}

// ArrType identifies which *arr application an instance belongs to.
type ArrType string

const (
	ArrTypeRadarr  ArrType = "radarr"
	ArrTypeSonarr  ArrType = "sonarr"
	ArrTypeLidarr  ArrType = "lidarr"
	ArrTypeReadarr ArrType = "readarr"
)

// ArrInstance holds connection info and webhook state for a single *arr app.
type ArrInstance struct {
	ID                string  `yaml:"id"                  json:"id"`
	Name              string  `yaml:"name"                json:"name"`
	Type              ArrType `yaml:"type"                json:"type"`
	URL               string  `yaml:"url"                 json:"url"`
	APIKey            string  `yaml:"api_key"             json:"api_key"`
	Enabled           bool    `yaml:"enabled"             json:"enabled"`
	WebhookID         int64   `yaml:"webhook_id"          json:"webhook_id"`
	DeleteAfterUpload bool    `yaml:"delete_after_upload" json:"delete_after_upload"`
}

// ArrConfig holds all configured *arr instances.
type ArrConfig struct {
	Instances []ArrInstance `yaml:"instances" json:"instances"`
}

// ProgressStatus represents the progress of file processing operations
type ProgressStatus struct {
	CurrentFile         string  `json:"currentFile"`
	TotalFiles          int     `json:"totalFiles"`
	CompletedFiles      int     `json:"completedFiles"`
	Stage               string  `json:"stage"`
	Details             string  `json:"details"`
	IsRunning           bool    `json:"isRunning"`
	LastUpdate          int64   `json:"lastUpdate"`
	Percentage          float64 `json:"percentage"`
	CurrentFileProgress float64 `json:"currentFileProgress"`
	JobID               string  `json:"jobID"`
	TotalBytes          int64   `json:"totalBytes"`
	TransferredBytes    int64   `json:"transferredBytes"`
	CurrentFileBytes    int64   `json:"currentFileBytes"`
	Speed               float64 `json:"speed"`
	SecondsLeft         float64 `json:"secondsLeft"`
	ElapsedTime         float64 `json:"elapsedTime"`
}

// IsConfigVersionTooNew checks if the config version is newer than the current version
func IsConfigVersionTooNew(configVersion int) bool {
	return configVersion > CurrentConfigVersion
}

// Load loads configuration from a file
func Load(path string) (*ConfigData, error) {
	enabled := true

	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("error reading config file: %w", err)
	}

	var cfg ConfigData
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("error parsing config file: %w", err)
	}

	// Reject configs from future versions that this binary cannot understand
	if IsConfigVersionTooNew(cfg.Version) {
		return nil, fmt.Errorf("config version %d is newer than supported version %d; please upgrade the application", cfg.Version, CurrentConfigVersion)
	}

	// Migrate v1 CheckOnly → v2 Role
	if cfg.Version <= 1 {
		slog.Info("Migrating config from v0/v1 to v2 (CheckOnly → Role)",
			"configPath", path)
		for i := range cfg.Servers {
			s := &cfg.Servers[i]
			if s.Role == "" {
				if s.CheckOnly != nil && *s.CheckOnly {
					s.Role = ServerRoleVerify
				} else {
					s.Role = ServerRoleUpload
				}
			}
			s.CheckOnly = nil // clear deprecated field
		}
		cfg.Version = CurrentConfigVersion

		// Persist the migrated config so future loads don't re-trigger migration
		if saveErr := SaveConfig(&cfg, path); saveErr != nil {
			slog.Warn("Failed to save migrated config to disk", "error", saveErr, "configPath", path)
		} else {
			slog.Info("Config migration saved successfully", "configPath", path)
		}
	}

	// Set default values
	if cfg.Posting.MaxRetries <= 0 {
		cfg.Posting.MaxRetries = 3
	}

	if cfg.Posting.RetryDelay == "" {
		cfg.Posting.RetryDelay = Duration("5s")
	}

	// Post-upload script defaults (YAML may omit these fields)
	if cfg.PostUploadScript.Timeout == "" {
		cfg.PostUploadScript.Timeout = Duration("30s")
	}
	if cfg.PostUploadScript.MaxRetries < 0 {
		cfg.PostUploadScript.MaxRetries = 3 // 0 means unlimited
	}
	if cfg.PostUploadScript.RetryDelay == "" {
		cfg.PostUploadScript.RetryDelay = Duration("30s")
	}
	if cfg.PostUploadScript.MaxBackoff == "" {
		cfg.PostUploadScript.MaxBackoff = Duration("1h")
	}
	if cfg.PostUploadScript.MaxRetryDuration == "" {
		cfg.PostUploadScript.MaxRetryDuration = Duration("24h")
	}
	if cfg.PostUploadScript.RetryCheckInterval == "" {
		cfg.PostUploadScript.RetryCheckInterval = Duration("1m")
	}

	if cfg.Posting.ArticleSizeInBytes <= 0 {
		cfg.Posting.ArticleSizeInBytes = 750000 // Default to 750KB
	}

	if cfg.Posting.ThrottleRate <= 0 {
		cfg.Posting.ThrottleRate = 0 // Default to unlimited
	}

	if cfg.Posting.GroupPolicy == "" {
		cfg.Posting.GroupPolicy = GroupPolicyEachFile
	}

	if cfg.Posting.MessageIDFormat == "" {
		cfg.Posting.MessageIDFormat = MessageIDFormatRandom
	}

	if cfg.Posting.ObfuscationPolicy == "" {
		cfg.Posting.ObfuscationPolicy = ObfuscationPolicyFull
	}

	if cfg.Par2.Enabled == nil {
		cfg.Par2.Enabled = &enabled
	}

	if cfg.Posting.WaitForPar2 == nil {
		cfg.Posting.WaitForPar2 = &enabled
	} else if !*cfg.Posting.WaitForPar2 {
		if !*cfg.Par2.Enabled {
			cfg.Posting.WaitForPar2 = &enabled
		} else {
			slog.Warn("Use it at your own risk. Par2 files will be created and uploaded in parallel with the original file, " +
				"if par2 creation fails or posting fails, you will end up uploading something that is trash.")
		}
	}

	if cfg.PostCheck.Enabled == nil {
		cfg.PostCheck.Enabled = &enabled
	}

	// Set defaults for deferred post check fields
	if cfg.PostCheck.DeferredCheckDelay == "" {
		cfg.PostCheck.DeferredCheckDelay = Duration("30s")
	}
	if cfg.PostCheck.DeferredMaxRetries == 0 {
		cfg.PostCheck.DeferredMaxRetries = 5
	}
	if cfg.PostCheck.DeferredMaxBackoff == "" {
		cfg.PostCheck.DeferredMaxBackoff = Duration("5m")
	}
	if cfg.PostCheck.DeferredCheckInterval == "" {
		cfg.PostCheck.DeferredCheckInterval = Duration("2m")
	}
	if cfg.PostCheck.DeferredBatchSize <= 0 {
		cfg.PostCheck.DeferredBatchSize = 10000
	}
	if cfg.PostCheck.StatBatchSize <= 0 {
		cfg.PostCheck.StatBatchSize = 100
	}

	if cfg.Par2.Redundancy == "" {
		cfg.Par2.Redundancy = defaultRedundancy
	}

	if cfg.Par2.MemoryLimit == 0 {
		cfg.Par2.MemoryLimit = 4 * 1024 * 1024 * 1024 // 4 GiB
	}

	if cfg.Par2.SliceSize == 0 {
		cfg.Par2.SliceSize = 10 * 1024 * 1024 // 10 MiB
	}

	// Process-wide PAR2 scheduler: default to a single concurrent job so the
	// configured MemoryLimit is not multiplied across simultaneous queue jobs.
	if cfg.Par2.MaxConcurrentJobs <= 0 {
		cfg.Par2.MaxConcurrentJobs = 1
	}

	if cfg.Par2.GF16Method == "" {
		cfg.Par2.GF16Method = Par2GF16MethodAuto
	}

	// Set default for maintain par2 files (default to false to preserve current behavior)
	if cfg.Par2.MaintainPar2Files == nil {
		maintainPar2Files := false
		cfg.Par2.MaintainPar2Files = &maintainPar2Files
	}

	// Set default for skip if par2 exists (default to true)
	if cfg.Par2.SkipIfPar2Exists == nil {
		skipIfPar2Exists := true
		cfg.Par2.SkipIfPar2Exists = &skipIfPar2Exists
	}

	// Set default values for NZB compression
	if cfg.NzbCompression.Type == "" {
		cfg.NzbCompression.Type = CompressionTypeNone
	}

	// Set default compression level based on type
	if cfg.NzbCompression.Level == 0 {
		switch cfg.NzbCompression.Type {
		case CompressionTypeZstd:
			cfg.NzbCompression.Level = 3 // Default zstd level
		case CompressionTypeBrotli:
			cfg.NzbCompression.Level = 4 // Default brotli level
		case CompressionTypeZip:
			cfg.NzbCompression.Level = 6 // Default zip level
		}
	}

	// Set default values for Database configuration
	if cfg.Database.DatabaseType == "" {
		cfg.Database.DatabaseType = "sqlite"
	}

	if cfg.Database.DatabasePath == "" {
		cfg.Database.DatabasePath = "./postie.db"
	}

	// Set default values for Queue configuration
	if cfg.Queue.MaxConcurrentUploads <= 0 {
		cfg.Queue.MaxConcurrentUploads = 1
	}

	// Set default for maintain original extension (default to true)
	if cfg.MaintainOriginalExtension == nil {
		cfg.MaintainOriginalExtension = &enabled
	}

	// Set default enabled state and role for servers
	for i := range cfg.Servers {
		if cfg.Servers[i].Enabled == nil {
			cfg.Servers[i].Enabled = &enabled
		}
		if cfg.Servers[i].Role == "" {
			cfg.Servers[i].Role = ServerRoleUpload
		}
	}

	// Migrate single watcher to watchers slice
	cfg.migrateWatcherConfig()

	// Validate configuration
	if err := cfg.Validate(); err != nil {
		return nil, fmt.Errorf("invalid configuration: %w", err)
	}

	return &cfg, nil
}

// Validate validates the configuration
func (c *ConfigData) Validate() error {
	for i, s := range c.Servers {
		if s.Host == "" {
			return fmt.Errorf("server %d: host is required", i)
		}

		if s.Port <= 0 || s.Port > 65535 {
			return fmt.Errorf("server %d: invalid port number", i)
		}

		if s.MaxConnections <= 0 {
			return fmt.Errorf("server %d: max_connections must be positive", i)
		}
	}

	// Validate that no two servers share the same host+username (would collide in nntppool)
	type serverKey struct{ host, username string }
	seen := make(map[serverKey]int)
	for i, s := range c.Servers {
		k := serverKey{s.Host, s.Username}
		if j, exists := seen[k]; exists {
			return fmt.Errorf("servers %d and %d have the same host (%q) and username (%q); each provider account must be unique", j+1, i+1, s.Host, s.Username)
		}
		seen[k] = i
	}

	// Validate upload servers: at least one required, all must share the same host
	uploadServers := c.GetUploadServers()
	if len(uploadServers) == 0 {
		return fmt.Errorf("at least one upload server is required")
	}
	if len(c.Posting.Groups) == 0 {
		return fmt.Errorf("posting groups are required")
	}

	// Validate compression configuration
	if c.NzbCompression.Enabled {
		switch c.NzbCompression.Type {
		case CompressionTypeZstd:
			// zstd levels are between 1-22
			if c.NzbCompression.Level < 1 || c.NzbCompression.Level > 22 {
				return fmt.Errorf("invalid zstd compression level: %d (must be between 1-22)", c.NzbCompression.Level)
			}
		case CompressionTypeBrotli:
			// brotli levels are between 0-11
			if c.NzbCompression.Level < 0 || c.NzbCompression.Level > 11 {
				return fmt.Errorf("invalid brotli compression level: %d (must be between 0-11)", c.NzbCompression.Level)
			}
		case CompressionTypeZip:
			// zip levels are between 0-9
			if c.NzbCompression.Level < 0 || c.NzbCompression.Level > 9 {
				return fmt.Errorf("invalid zip compression level: %d (must be between 0-9)", c.NzbCompression.Level)
			}
		case CompressionTypeNone:
			// Do nothing
		default:
			return fmt.Errorf("invalid compression type: %s", c.NzbCompression.Type)
		}
	}

	// Validate database configuration
	switch c.Database.DatabaseType {
	case "sqlite", "postgres", "mysql":
		// Valid database types
	default:
		return fmt.Errorf("invalid database type: %s (supported: sqlite, postgres, mysql)", c.Database.DatabaseType)
	}

	// Validate queue configuration
	if c.Queue.MaxConcurrentUploads <= 0 {
		return fmt.Errorf("queue max concurrent uploads must be positive")
	}

	// Validate process-wide resource limits (0 means automatic sizing).
	if c.Posting.UploadBufferMemoryLimit < 0 {
		return fmt.Errorf("posting upload_buffer_memory_limit must be >= 0 (0 = auto)")
	}
	if c.Par2.MaxConcurrentJobs < 0 {
		return fmt.Errorf("par2 max_concurrent_jobs must be >= 0 (0 = auto)")
	}
	if c.Par2.GF16Method != "" && !slices.Contains(Par2GF16Methods, c.Par2.GF16Method) {
		return fmt.Errorf("invalid par2 gf16_method: %q (accepted: %s)", c.Par2.GF16Method, strings.Join(Par2GF16Methods, ", "))
	}
	if c.PostCheck.MaxConcurrentChecks < 0 {
		return fmt.Errorf("post_check max_concurrent_checks must be >= 0 (0 = auto)")
	}

	// Validate DefaultFrom is a valid RFC 2822 address if set
	if c.Posting.PostHeaders.DefaultFrom != "" {
		if _, err := mail.ParseAddress(c.Posting.PostHeaders.DefaultFrom); err != nil {
			return fmt.Errorf("posting post_headers default_from is not a valid email address: %w", err)
		}
	}

	return nil
}

// GetUploadServers returns enabled servers with the upload role (used for posting articles).
func (c *ConfigData) GetUploadServers() []ServerConfig {
	var servers []ServerConfig
	for _, s := range c.Servers {
		isEnabled := s.Enabled == nil || *s.Enabled
		if isEnabled && s.Role != ServerRoleVerify {
			servers = append(servers, s)
		}
	}
	return servers
}

// GetVerifyServers returns enabled servers with the verify role (used only for STAT checks).
func (c *ConfigData) GetVerifyServers() []ServerConfig {
	var servers []ServerConfig
	for _, s := range c.Servers {
		isEnabled := s.Enabled == nil || *s.Enabled
		if isEnabled && s.Role == ServerRoleVerify {
			servers = append(servers, s)
		}
	}
	return servers
}

// GetPostingServers is a backward-compatible alias for GetUploadServers.
func (c *ConfigData) GetPostingServers() []ServerConfig {
	return c.GetUploadServers()
}

// GetCheckOnlyServers is a backward-compatible alias for GetVerifyServers.
func (c *ConfigData) GetCheckOnlyServers() []ServerConfig {
	return c.GetVerifyServers()
}

// ServerConfigToProvider converts a ServerConfig to nntppool.Provider
func ServerConfigToProvider(s ServerConfig) nntppool.Provider {
	maxConnections := s.MaxConnections
	if maxConnections <= 0 {
		maxConnections = 10 // default value if not specified
	}

	idleTimeout := time.Duration(s.MaxConnectionIdleTimeInSeconds) * time.Second
	if idleTimeout <= 0 {
		idleTimeout = 300 * time.Second
	}

	keepAlive := time.Duration(s.MaxConnectionTTLInSeconds) * time.Second
	if keepAlive <= 0 {
		keepAlive = 3600 * time.Second
	}

	addr := fmt.Sprintf("%s:%d", s.Host, s.Port)

	inflight := s.Inflight
	if inflight <= 0 {
		inflight = 10
	}

	provider := nntppool.Provider{
		Host:        addr,
		Auth:        nntppool.Auth{Username: s.Username, Password: s.Password},
		Connections: maxConnections,
		Inflight:    inflight,
		IdleTimeout: idleTimeout,
		KeepAlive:   keepAlive,
	}

	if s.SSL {
		provider.TLSConfig = &tls.Config{
			InsecureSkipVerify: s.InsecureSSL, //nolint:gosec // user-configurable option
			ServerName:         s.Host,
		}
	}

	// Support SOCKS5 proxy via custom connection factory
	if s.ProxyURL != "" {
		proxyURL := s.ProxyURL
		useTLS := s.SSL
		tlsCfg := provider.TLSConfig
		provider.Factory = func(ctx context.Context) (net.Conn, error) {
			dialer, err := proxy.SOCKS5("tcp", proxyURL, nil, proxy.Direct)
			if err != nil {
				return nil, fmt.Errorf("failed to create SOCKS5 dialer: %w", err)
			}
			conn, err := dialer.Dial("tcp", addr)
			if err != nil {
				return nil, fmt.Errorf("failed to connect via proxy: %w", err)
			}
			if useTLS && tlsCfg != nil {
				tlsConn := tls.Client(conn, tlsCfg)
				if err := tlsConn.HandshakeContext(ctx); err != nil {
					_ = conn.Close()
					return nil, fmt.Errorf("TLS handshake via proxy failed: %w", err)
				}
				return tlsConn, nil
			}
			return conn, nil
		}
		// Clear TLSConfig since the factory handles TLS
		provider.TLSConfig = nil
	}

	return provider
}

// getProviders converts server configs to nntppool providers
func getProviders(servers []ServerConfig) []nntppool.Provider {
	providers := make([]nntppool.Provider, len(servers))
	for i, s := range servers {
		providers[i] = ServerConfigToProvider(s)
	}
	return providers
}

// GetNNTPPool returns the NNTP client (all enabled servers)
func (c *ConfigData) GetNNTPPool() (*nntppool.Client, error) {
	var enabledServers []ServerConfig
	for _, s := range c.Servers {
		if s.Enabled == nil || *s.Enabled {
			enabledServers = append(enabledServers, s)
		}
	}

	providers := getProviders(enabledServers)

	client, err := nntppool.NewClient(context.Background(), providers, nntppool.WithDispatchStrategy(nntppool.DispatchRoundRobin))
	if err != nil {
		return nil, fmt.Errorf("error creating NNTP client: %w", err)
	}

	return client, nil
}

// GetUploadPool returns the NNTP client for posting articles (upload-role servers only).
func (c *ConfigData) GetUploadPool() (*nntppool.Client, error) {
	uploadServers := c.GetUploadServers()
	if len(uploadServers) == 0 {
		return nil, fmt.Errorf("no upload servers configured")
	}

	providers := getProviders(uploadServers)

	client, err := nntppool.NewClient(context.Background(), providers, nntppool.WithDispatchStrategy(nntppool.DispatchRoundRobin))
	if err != nil {
		return nil, fmt.Errorf("error creating upload pool: %w", err)
	}

	return client, nil
}

// GetVerifyPool returns the NNTP client for article verification.
// Uses verify-role servers if available, otherwise falls back to upload servers.
func (c *ConfigData) GetVerifyPool() (*nntppool.Client, error) {
	verifyServers := c.GetVerifyServers()

	if len(verifyServers) > 0 {
		providers := getProviders(verifyServers)
		client, err := nntppool.NewClient(context.Background(), providers, nntppool.WithDispatchStrategy(nntppool.DispatchRoundRobin))
		if err != nil {
			return nil, fmt.Errorf("error creating verify pool: %w", err)
		}
		return client, nil
	}

	// Fall back to upload servers for verification
	return c.GetUploadPool()
}

// GetPostingPool is a backward-compatible alias for GetUploadPool.
func (c *ConfigData) GetPostingPool() (*nntppool.Client, error) {
	return c.GetUploadPool()
}

// GetCheckPool is a backward-compatible alias for GetVerifyPool.
func (c *ConfigData) GetCheckPool() (*nntppool.Client, error) {
	return c.GetVerifyPool()
}

func (c *ConfigData) GetPar2Config(_ context.Context) (*Par2Config, error) {
	return &c.Par2, nil
}

func (c *ConfigData) GetPostingConfig() PostingConfig {
	return c.Posting
}

func (c *ConfigData) GetPostCheckConfig() PostCheck {
	return c.PostCheck
}

// GetWatcherConfig returns the first watcher config (legacy compatibility).
func (c *ConfigData) GetWatcherConfig() WatcherConfig {
	if len(c.Watchers) > 0 {
		return c.Watchers[0]
	}
	return c.Watcher
}

// GetWatcherConfigs returns all watcher configurations.
func (c *ConfigData) GetWatcherConfigs() []WatcherConfig {
	return c.Watchers
}

// migrateWatcherConfig migrates the old single-watcher config to the new multi-watcher format.
func (c *ConfigData) migrateWatcherConfig() {
	if len(c.Watchers) == 0 && c.Watcher.WatchDirectory != "" {
		c.Watchers = []WatcherConfig{c.Watcher}
		c.Watcher = WatcherConfig{} // clear deprecated field
	}
	// If still empty, add one default disabled entry
	if len(c.Watchers) == 0 {
		c.Watchers = []WatcherConfig{defaultWatcherConfig()}
	}
}

// defaultWatcherConfig returns a default WatcherConfig.
func defaultWatcherConfig() WatcherConfig {
	return WatcherConfig{
		Name:           "",
		Enabled:        false,
		WatchDirectory: "",
		SizeThreshold:  104857600, // 100MB
		Schedule: ScheduleConfig{
			StartTime: "00:00",
			EndTime:   "23:59",
		},
		IgnorePatterns:     []string{"*.tmp", "*.part", "*.!ut"},
		MinFileSize:        1048576, // 1MB
		CheckInterval:      Duration("5m"),
		DeleteOriginalFile: false,
		SingleNzbPerFolder: false,
		FollowSymlinks:     false,
		MinFileAge:         Duration("60s"),
		MinFileAgeToDelete: Duration("0s"),
	}
}

func (c *ConfigData) GetNzbCompressionConfig() NzbCompressionConfig {
	return c.NzbCompression
}

func (c *ConfigData) GetDatabaseConfig() DatabaseConfig {
	return c.Database
}

func (c *ConfigData) GetQueueConfig() QueueConfig {
	return c.Queue
}

func (c *ConfigData) GetAPIConfig() APIConfig {
	return c.API
}

func (c *ConfigData) GetOutputDir() string {
	if c.OutputDir != "" {
		return c.OutputDir
	}

	// Default to "./output" if not configured
	return "./output"
}

// GetDefaultConfig returns a default configuration
func GetDefaultConfig() ConfigData {
	enabled := true
	disabled := false
	return ConfigData{
		Version: CurrentConfigVersion,
		Servers: []ServerConfig{},
		ConnectionPool: ConnectionPoolConfig{
			MinConnections:      0,
			HealthCheckInterval: Duration("1m"),
		},
		Posting: PostingConfig{
			WaitForPar2:        &enabled,
			MaxRetries:         3,
			RetryDelay:         Duration("5s"),
			ArticleSizeInBytes: 768000, // 768KB
			Groups: []NewsgroupConfig{
				{Name: "alt.binaries.test", Enabled: &enabled},
			},
			ThrottleRate:    0, // 0 means no throttling
			MessageIDFormat: MessageIDFormatRandom,
			PostHeaders: PostHeaders{
				AddNXGHeader:  false,
				DefaultFrom:   "",
				CustomHeaders: []CustomHeader{},
			},
			ObfuscationPolicy:     ObfuscationPolicyFull,
			Par2ObfuscationPolicy: ObfuscationPolicyFull,
			GroupPolicy:           GroupPolicyEachFile,
		},
		PostCheck: PostCheck{
			Enabled:               &enabled,
			RetryDelay:            Duration("10s"),
			MaxRePost:             1,
			DeferredCheckDelay:    Duration("30s"),
			DeferredMaxRetries:    5,
			DeferredMaxBackoff:    Duration("5m"),
			DeferredCheckInterval: Duration("2m"),
			DeferredBatchSize:     10000,
			StatBatchSize:         100,
		},
		Par2: Par2Config{
			Enabled:           &enabled,
			Redundancy:        defaultRedundancy,
			TempDir:           os.TempDir(),
			MaintainPar2Files: &disabled, // Default to false to preserve current behavior
			MemoryLimit:       4 * 1024 * 1024 * 1024,
			SliceSize:         10 * 1024 * 1024,
			MaxConcurrentJobs: 1,
			GF16Method:        Par2GF16MethodAuto,
		},
		Watchers: []WatcherConfig{
			{
				Name:           "",
				Enabled:        false,
				WatchDirectory: "",        // Will be set to default in backend if empty
				SizeThreshold:  104857600, // 100MB
				Schedule: ScheduleConfig{
					StartTime: "00:00",
					EndTime:   "23:59",
				},
				IgnorePatterns:     []string{"*.tmp", "*.part", "*.!ut"},
				MinFileSize:        1048576, // 1MB
				CheckInterval:      Duration("5m"),
				DeleteOriginalFile: false,           // Default to keeping original files for safety
				SingleNzbPerFolder: false,           // Default to false for backward compatibility
				FollowSymlinks:     false,           // Default to skipping symlinks to avoid double-counting and external files
				MinFileAge:         Duration("60s"), // Default to 60s to ensure files are stable before uploading
				MinFileAgeToDelete: Duration("0s"),  // Default to 0s (delete immediately)
			},
		},
		NzbCompression: NzbCompressionConfig{
			Enabled: disabled,
			Type:    CompressionTypeNone,
			Level:   0,
		},
		Database: DatabaseConfig{
			DatabaseType: "sqlite",
			DatabasePath: "./postie.db",
		},
		Queue: QueueConfig{
			MaxConcurrentUploads: 1,
		},
		OutputDir:                 "./output",
		MaintainOriginalExtension: &enabled,
		PostUploadScript: PostUploadScriptConfig{
			Enabled:            false,
			Command:            "",
			Timeout:            Duration("30s"),
			MaxRetries:         3,
			RetryDelay:         Duration("30s"),
			MaxBackoff:         Duration("1h"),
			MaxRetryDuration:   Duration("24h"),
			RetryCheckInterval: Duration("1m"),
		},
	}
}

// SaveConfig saves a ConfigData to a file
func SaveConfig(configData *ConfigData, path string) error {
	data, err := yaml.Marshal(configData)
	if err != nil {
		return fmt.Errorf("invalid configuration format: %v", err)
	}

	if err := os.WriteFile(path, data, 0644); err != nil {
		return err // Let caller wrap with path context and user-friendly message
	}

	return nil
}

func (c *ConfigData) GetPostUploadScriptConfig() PostUploadScriptConfig {
	return c.PostUploadScript
}

func (c *ConfigData) GetMaintainOriginalExtension() bool {
	if c.MaintainOriginalExtension == nil {
		return true // Default to true
	}
	return *c.MaintainOriginalExtension
}
