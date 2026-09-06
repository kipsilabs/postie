package processor

import (
	"testing"

	"github.com/javi11/nntppool/v4"
)

func TestEvaluateProviderAvailability(t *testing.T) {
	tests := []struct {
		name        string
		baseline    map[string]int64
		providers   []nntppool.ProviderStats
		wantHealthy int
	}{
		{
			// Regression for #172: ProviderStats.Errors is cumulative since client
			// start, so a single historical error used to mark an idle provider
			// unavailable forever. Blocking new jobs then guaranteed
			// ActiveConnections stayed 0, so the block could never lift.
			name:        "idle provider with only historical errors stays healthy",
			baseline:    map[string]int64{"news.example.com": 5},
			providers:   []nntppool.ProviderStats{{Name: "news.example.com", Errors: 5, ActiveConnections: 0}},
			wantHealthy: 1,
		},
		{
			name:        "idle provider accruing new errors is unhealthy",
			baseline:    map[string]int64{"news.example.com": 5},
			providers:   []nntppool.ProviderStats{{Name: "news.example.com", Errors: 9, ActiveConnections: 0}},
			wantHealthy: 0,
		},
		{
			name:        "provider with live connections is healthy despite new errors",
			baseline:    map[string]int64{"news.example.com": 5},
			providers:   []nntppool.ProviderStats{{Name: "news.example.com", Errors: 9, ActiveConnections: 3}},
			wantHealthy: 1,
		},
		{
			// Errors accumulated before monitoring started say nothing about
			// current reachability; the first observation only seeds the baseline.
			name:        "first observation seeds the baseline without blocking",
			baseline:    map[string]int64{},
			providers:   []nntppool.ProviderStats{{Name: "news.example.com", Errors: 100, ActiveConnections: 0}},
			wantHealthy: 1,
		},
		{
			name:     "healthy providers are counted independently of failing ones",
			baseline: map[string]int64{"good.example.com": 0, "bad.example.com": 2},
			providers: []nntppool.ProviderStats{
				{Name: "good.example.com", Errors: 0, ActiveConnections: 0},
				{Name: "bad.example.com", Errors: 7, ActiveConnections: 0},
			},
			wantHealthy: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			stats := nntppool.ClientStats{Providers: tt.providers}

			healthy, next := evaluateProviderAvailability(stats, tt.baseline)

			if healthy != tt.wantHealthy {
				t.Errorf("healthy providers = %d; want %d", healthy, tt.wantHealthy)
			}
			for _, p := range tt.providers {
				if got := next[p.Name]; got != p.Errors {
					t.Errorf("baseline[%q] = %d; want %d (current cumulative count)", p.Name, got, p.Errors)
				}
			}
		})
	}
}

// The monitor's baseline field starts as a nil map, so the first real call
// always passes nil rather than an empty map.
func TestEvaluateProviderAvailabilityWithNilBaseline(t *testing.T) {
	stats := nntppool.ClientStats{
		Providers: []nntppool.ProviderStats{{Name: "news.example.com", Errors: 42}},
	}

	healthy, next := evaluateProviderAvailability(stats, nil)

	if healthy != 1 {
		t.Errorf("healthy providers = %d; want 1 (first observation only seeds the baseline)", healthy)
	}
	if got := next["news.example.com"]; got != 42 {
		t.Errorf("baseline = %d; want 42", got)
	}
}

func TestEvaluateProviderAvailabilityWithNoProviders(t *testing.T) {
	healthy, next := evaluateProviderAvailability(nntppool.ClientStats{}, nil)

	if healthy != 0 {
		t.Errorf("healthy providers = %d; want 0", healthy)
	}
	if len(next) != 0 {
		t.Errorf("baseline has %d entries; want 0", len(next))
	}
}

func TestEvaluateProviderAvailabilityForgetsRemovedProviders(t *testing.T) {
	baseline := map[string]int64{"removed.example.com": 4}
	stats := nntppool.ClientStats{
		Providers: []nntppool.ProviderStats{{Name: "kept.example.com", Errors: 1}},
	}

	_, next := evaluateProviderAvailability(stats, baseline)

	if _, ok := next["removed.example.com"]; ok {
		t.Error("baseline retained a provider that is no longer configured")
	}
}
