//! Port of `provider_availability.go`: decides how many providers look reachable.
//!
//! The error counter is cumulative since client start, so it is only a health signal when compared
//! with the previous observation. A provider is reachable when it holds live connections or recorded
//! no new errors since the last check; one seen for the first time only seeds the baseline. A
//! decreasing counter (a server edited in settings re-adds the provider with fresh stats) reads as "no
//! new errors", giving the reconfigured provider a clean slate.

use std::collections::HashMap;

/// One provider's observed health counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHealth {
    pub name: String,
    pub active_connections: i64,
    pub errors: i64,
}

/// Returns the healthy count and the baseline to pass to the next call.
pub fn evaluate_provider_availability(
    stats: &[ProviderHealth],
    baseline: &HashMap<String, i64>,
) -> (usize, HashMap<String, i64>) {
    let mut next = HashMap::with_capacity(stats.len());
    let mut healthy = 0;
    for p in stats {
        let previous = baseline.get(&p.name);
        next.insert(p.name.clone(), p.errors);
        if p.active_connections > 0 || previous.is_none_or(|prev| p.errors <= *prev) {
            healthy += 1;
        }
    }
    (healthy, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, active: i64, errors: i64) -> ProviderHealth {
        ProviderHealth {
            name: name.into(),
            active_connections: active,
            errors,
        }
    }

    #[test]
    fn first_sight_only_seeds_the_baseline() {
        let (healthy, base) = evaluate_provider_availability(&[p("a", 0, 99)], &HashMap::new());
        assert_eq!((healthy, base["a"]), (1, 99));
    }

    #[test]
    fn new_errors_without_connections_are_unhealthy_but_connections_win() {
        let base = HashMap::from([
            ("a".to_string(), 5),
            ("b".to_string(), 5),
            ("c".to_string(), 5),
        ]);
        let (healthy, next) =
            evaluate_provider_availability(&[p("a", 0, 6), p("b", 2, 50), p("c", 0, 5)], &base);
        assert_eq!(
            healthy, 2,
            "a is down; b has connections; c has no new errors"
        );
        assert_eq!((next["a"], next["b"]), (6, 50));
    }

    #[test]
    fn a_decreasing_counter_is_a_clean_slate_and_removed_providers_drop_from_the_baseline() {
        let base = HashMap::from([("a".to_string(), 100), ("gone".to_string(), 1)]);
        let (healthy, next) = evaluate_provider_availability(&[p("a", 0, 3)], &base);
        assert_eq!(healthy, 1);
        assert!(!next.contains_key("gone"));
        assert_eq!(evaluate_provider_availability(&[], &base).0, 0);
    }
}
