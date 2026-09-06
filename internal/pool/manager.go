package pool

import (
	"fmt"
	"log/slog"
	"sync"

	"github.com/javi11/nntppool/v4"
	"github.com/kipsilabs/postie/internal/config"
)

// PoolManager defines the interface for connection pool management.
// This interface is implemented by Manager and can be mocked for testing.
type PoolManager interface {
	GetUploadPool() NNTPClient
	GetVerifyPool() NNTPClient
	GetPool() NNTPClient      // Deprecated: use GetUploadPool
	GetCheckPool() NNTPClient // Deprecated: use GetVerifyPool
}

// Manager manages NNTP connection pools throughout the application lifecycle,
// enabling proper metrics accumulation. Use dependency injection to pass this around.
// Supports separate pools for posting (upload) and article verification (verify).
type Manager struct {
	uploadPool NNTPClient // Pool for posting (upload-role servers)
	verifyPool NNTPClient // Pool for article verification (verify-role servers, or fallback to upload)
	config     *config.ConfigData
	mu         sync.RWMutex
	closed     bool
}

// New creates a new connection pool manager with the given configuration.
// Creates separate pools for upload and verification.
func New(cfg *config.ConfigData) (*Manager, error) {
	if cfg == nil {
		return nil, fmt.Errorf("configuration cannot be nil")
	}

	slog.Info("Creating NNTP connection pool manager")

	// Create the upload pool
	uploadPool, err := cfg.GetUploadPool()
	if err != nil {
		return nil, fmt.Errorf("failed to create upload pool: %w", err)
	}

	// Create the verify pool (verify-role servers, or falls back to upload servers)
	verifyPool, err := cfg.GetVerifyPool()
	if err != nil {
		// If verify pool fails, fall back to upload pool
		slog.Warn("Failed to create dedicated verify pool, will use upload pool for article verification", "error", err)
		verifyPool = uploadPool
	}

	// Log pool configuration
	uploadServers := cfg.GetUploadServers()
	verifyServers := cfg.GetVerifyServers()
	uploadHost := ""
	if len(uploadServers) > 0 {
		uploadHost = uploadServers[0].Host
	}
	slog.Info("NNTP connection pools configured",
		"upload_servers", len(uploadServers),
		"verify_servers", len(verifyServers),
		"upload_host", uploadHost,
		"using_dedicated_verify_pool", len(verifyServers) > 0)

	manager := &Manager{
		uploadPool: uploadPool,
		verifyPool: verifyPool,
		config:     cfg,
		closed:     false,
	}

	slog.Info("NNTP connection pool manager created successfully")
	return manager, nil
}

// GetUploadPool returns the NNTP connection pool for posting articles.
func (m *Manager) GetUploadPool() NNTPClient {
	m.mu.RLock()
	defer m.mu.RUnlock()

	return m.uploadPool
}

// GetVerifyPool returns the NNTP connection pool for article verification.
// Returns verify-role servers if configured, otherwise falls back to upload pool.
func (m *Manager) GetVerifyPool() NNTPClient {
	m.mu.RLock()
	defer m.mu.RUnlock()

	if m.verifyPool != nil {
		return m.verifyPool
	}
	return m.uploadPool
}

// GetPool is a deprecated alias for GetUploadPool.
func (m *Manager) GetPool() NNTPClient {
	return m.GetUploadPool()
}

// GetCheckPool is a deprecated alias for GetVerifyPool.
func (m *Manager) GetCheckPool() NNTPClient {
	return m.GetVerifyPool()
}

// providerKey returns a unique key for a server config matching nntppool's internal provider name format.
func providerKey(s config.ServerConfig) string {
	if s.Username != "" {
		return fmt.Sprintf("%s:%d+%s", s.Host, s.Port, s.Username)
	}
	return fmt.Sprintf("%s:%d", s.Host, s.Port)
}

// UpdateConfig updates the connection pools with a new configuration.
// Uses AddProvider/RemoveProvider for incremental updates instead of destroying pools.
func (m *Manager) UpdateConfig(newCfg *config.ConfigData) error {
	if newCfg == nil {
		return fmt.Errorf("new configuration cannot be nil")
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	if m.closed {
		return fmt.Errorf("connection pool manager has been closed")
	}

	slog.Info("Updating NNTP connection pools with new configuration")

	// If pools don't exist yet, build them in place while holding the lock so
	// concurrent Close calls can't race with us. Close any leftover pools
	// before overwriting to avoid leaks (defensive — should not normally
	// happen when m.uploadPool == nil).
	if m.uploadPool == nil {
		uploadPool, err := newCfg.GetUploadPool()
		if err != nil {
			return fmt.Errorf("failed to create upload pool: %w", err)
		}
		verifyPool, err := newCfg.GetVerifyPool()
		if err != nil {
			slog.Warn("Failed to create dedicated verify pool, will use upload pool", "error", err)
			verifyPool = uploadPool
		}

		if m.verifyPool != nil && m.verifyPool != m.uploadPool {
			_ = m.verifyPool.Close()
		}
		// m.uploadPool is nil here by branch condition, but guard for safety.
		if m.uploadPool != nil {
			_ = m.uploadPool.Close()
		}

		m.uploadPool = uploadPool
		m.verifyPool = verifyPool
		m.config = newCfg
		return nil
	}

	// Diff upload providers
	oldUploadServers := m.config.GetUploadServers()
	newUploadServers := newCfg.GetUploadServers()
	if err := m.diffProviders(m.uploadPool, oldUploadServers, newUploadServers, "upload"); err != nil {
		return fmt.Errorf("upload pool update failed: %w", err)
	}

	// Diff verify providers
	oldVerifyServers := m.config.GetVerifyServers()
	newVerifyServers := newCfg.GetVerifyServers()

	// Handle verify pool transitions
	hasOldVerify := len(oldVerifyServers) > 0
	hasNewVerify := len(newVerifyServers) > 0

	switch {
	case hasNewVerify && hasOldVerify && m.verifyPool != m.uploadPool:
		// Both old and new have dedicated verify pools — diff the verify pool
		if err := m.diffProviders(m.verifyPool, oldVerifyServers, newVerifyServers, "verify"); err != nil {
			return fmt.Errorf("verify pool update failed: %w", err)
		}
	case hasNewVerify && !hasOldVerify:
		// New config introduces verify servers — create dedicated verify pool
		verifyPool, err := newCfg.GetVerifyPool()
		if err != nil {
			slog.Warn("Failed to create dedicated verify pool, will use upload pool", "error", err)
			m.verifyPool = m.uploadPool
		} else {
			m.verifyPool = verifyPool
		}
	case !hasNewVerify && hasOldVerify:
		// Verify servers removed — close dedicated verify pool, fall back to upload pool
		if m.verifyPool != nil && m.verifyPool != m.uploadPool {
			_ = m.verifyPool.Close()
		}
		m.verifyPool = m.uploadPool
	default:
		// No verify servers in either config — verifyPool stays as upload pool alias
		m.verifyPool = m.uploadPool
	}

	m.config = newCfg

	slog.Info("NNTP connection pools updated successfully",
		"upload_servers", len(newUploadServers),
		"verify_servers", len(newVerifyServers))

	return nil
}

// diffProviders computes the diff between old and new server lists and applies
// AddProvider/RemoveProvider operations on the given pool.
func (m *Manager) diffProviders(pool NNTPClient, oldServers, newServers []config.ServerConfig, poolName string) error {
	oldMap := make(map[string]config.ServerConfig, len(oldServers))
	for _, s := range oldServers {
		oldMap[providerKey(s)] = s
	}

	newMap := make(map[string]config.ServerConfig, len(newServers))
	for _, s := range newServers {
		newMap[providerKey(s)] = s
	}

	// Remove providers that are no longer in the new config or have changed
	for key, oldSrv := range oldMap {
		newSrv, exists := newMap[key]
		if !exists || serverConfigChanged(oldSrv, newSrv) {
			if err := pool.RemoveProvider(providerKey(oldSrv)); err != nil {
				slog.Warn("Failed to remove provider from pool",
					"pool", poolName, "provider", key, "error", err)
			} else {
				slog.Info("Removed provider from pool", "pool", poolName, "provider", key)
			}
		}
	}

	// Add providers that are new or have changed
	for key, newSrv := range newMap {
		oldSrv, exists := oldMap[key]
		if !exists || serverConfigChanged(oldSrv, newSrv) {
			provider := config.ServerConfigToProvider(newSrv)
			if err := pool.AddProvider(provider); err != nil {
				return fmt.Errorf("failed to add provider %q to %s pool: %w", key, poolName, err)
			}
			slog.Info("Added provider to pool", "pool", poolName, "provider", key)
		}
	}
	return nil
}

// ServerConfigChanged returns true if any provider-relevant fields differ between two server configs.
// This is the canonical predicate for "did this provider's pool-affecting state change".
func ServerConfigChanged(a, b config.ServerConfig) bool {
	return serverConfigChanged(a, b)
}

// ProviderKey returns the canonical provider key for a server config.
func ProviderKey(s config.ServerConfig) string {
	return providerKey(s)
}

// serverConfigChanged returns true if any relevant fields differ between two server configs.
func serverConfigChanged(a, b config.ServerConfig) bool {
	if a.Host != b.Host || a.Port != b.Port {
		return true
	}
	if a.Username != b.Username || a.Password != b.Password {
		return true
	}
	if a.SSL != b.SSL || a.InsecureSSL != b.InsecureSSL {
		return true
	}
	if a.MaxConnections != b.MaxConnections {
		return true
	}
	if a.Inflight != b.Inflight {
		return true
	}
	if a.MaxConnectionIdleTimeInSeconds != b.MaxConnectionIdleTimeInSeconds {
		return true
	}
	if a.MaxConnectionTTLInSeconds != b.MaxConnectionTTLInSeconds {
		return true
	}
	if a.ProxyURL != b.ProxyURL {
		return true
	}
	aEnabled := a.Enabled == nil || *a.Enabled
	bEnabled := b.Enabled == nil || *b.Enabled
	if aEnabled != bEnabled {
		return true
	}
	if a.Role != b.Role {
		return true
	}
	return false
}

// GetMetrics returns the current metrics from the connection pool.
func (m *Manager) GetMetrics() (nntppool.ClientStats, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()

	if m.closed {
		return nntppool.ClientStats{}, fmt.Errorf("connection pool manager has been closed")
	}

	if m.uploadPool == nil {
		return nntppool.ClientStats{}, fmt.Errorf("connection pool is not available")
	}

	return m.uploadPool.Stats(), nil
}

// Close gracefully shuts down the connection pool manager.
// This should be called during application shutdown.
func (m *Manager) Close() error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.closed {
		return nil // Already closed
	}

	slog.Info("Closing NNTP connection pool manager")

	// Close verify pool first if it's different from the upload pool
	if m.verifyPool != nil && m.verifyPool != m.uploadPool {
		_ = m.verifyPool.Close()
		m.verifyPool = nil
	}

	if m.uploadPool != nil {
		_ = m.uploadPool.Close()
		m.uploadPool = nil
	}

	m.closed = true
	slog.Info("NNTP connection pool manager closed successfully")

	return nil
}

// IsClosed returns true if the manager has been closed.
func (m *Manager) IsClosed() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.closed
}
