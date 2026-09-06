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
