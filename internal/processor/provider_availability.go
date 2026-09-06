package processor

import "github.com/javi11/nntppool/v4"

// evaluateProviderAvailability counts how many providers look reachable and
// returns the error baseline to compare against on the next check.
//
// ProviderStats.Errors is cumulative since client start, so it can only be read
// as a health signal by comparing it against the previous observation. A
// provider is considered reachable when it either holds live connections or has
// recorded no new errors since the last check. Providers seen for the first
// time only seed the baseline: errors from before monitoring started say
// nothing about current reachability.
//
// The counter can also go down: editing a server in the settings makes
// Manager.UpdateConfig remove and re-add the provider under the same key, which
// installs a fresh stats struct. A decrease is read as "no new errors", giving
// the reconfigured provider a clean slate. The cost is that errors accrued
// between such a reset and the next check can be masked for a single cycle,
// which resolves itself because the baseline is replaced on every call.
func evaluateProviderAvailability(stats nntppool.ClientStats, baseline map[string]int64) (int, map[string]int64) {
	next := make(map[string]int64, len(stats.Providers))
	healthy := 0

	for _, provider := range stats.Providers {
		previous, seen := baseline[provider.Name]
		next[provider.Name] = provider.Errors

		if provider.ActiveConnections > 0 || !seen || provider.Errors <= previous {
			healthy++
		}
	}

	return healthy, next
}
