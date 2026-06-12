use crate::config::RoleConfig;
use crate::event::Provider;
use crate::quota::{unix_now, QuotaStore};

const ROUTING_WINDOW_SECS: i64 = 5 * 3600;

/// Picks the worker (index into `workers`) whose provider consumed the fewest
/// input tokens in the routing window. Ties break by config order. M2-simple:
/// token volume is a proxy for remaining quota headroom; per-pool $-budgets
/// arrive with M3 dashboards.
pub fn pick_worker(workers: &[RoleConfig], store: &QuotaStore) -> anyhow::Result<usize> {
    let providers: Vec<Provider> = workers.iter().map(|w| w.provider).collect();
    pick_worker_by_provider(&providers, store)
}

/// Same policy (least input tokens in the window, strict `<` so ties break by
/// slice order), over bare providers — conduct routes over CouncilMember
/// adapters whose RoleConfig isn't available.
/// TODO(M3): exclude providers whose pool is exhausted (per-pool $-budgets and
/// quota-error feedback), not just least-recently-used.
pub fn pick_worker_by_provider(
    providers: &[Provider],
    store: &QuotaStore,
) -> anyhow::Result<usize> {
    if providers.is_empty() {
        anyhow::bail!("no workers configured");
    }
    let since = unix_now() - ROUTING_WINDOW_SECS;
    let mut best = 0usize;
    let mut best_load = u64::MAX;
    for (i, p) in providers.iter().enumerate() {
        let (input, _) = store.totals_since(*p, since)?;
        if input < best_load {
            best_load = input;
            best = i;
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(p: Provider) -> RoleConfig {
        RoleConfig::new(p, "m")
    }

    #[test]
    fn picks_least_loaded_worker() {
        let store = QuotaStore::open_in_memory().unwrap();
        store.record(Provider::Codex, 10_000, 100).unwrap();
        store.record(Provider::Gemini, 50, 5).unwrap();
        let workers = vec![role(Provider::Codex), role(Provider::Gemini)];
        assert_eq!(pick_worker(&workers, &store).unwrap(), 1);
    }

    #[test]
    fn ties_break_by_config_order() {
        let store = QuotaStore::open_in_memory().unwrap();
        let workers = vec![role(Provider::Codex), role(Provider::Gemini)];
        assert_eq!(pick_worker(&workers, &store).unwrap(), 0);
    }

    #[test]
    fn old_usage_outside_window_ignored() {
        let store = QuotaStore::open_in_memory().unwrap();
        store
            .record_at(Provider::Codex, 999_999, 0, unix_now() - 10 * 3600)
            .unwrap();
        store.record(Provider::Gemini, 100, 10).unwrap();
        let workers = vec![role(Provider::Codex), role(Provider::Gemini)];
        assert_eq!(pick_worker(&workers, &store).unwrap(), 0);
    }

    #[test]
    fn empty_workers_is_error() {
        let store = QuotaStore::open_in_memory().unwrap();
        assert!(pick_worker(&[], &store).is_err());
    }

    #[test]
    fn by_provider_picks_least_loaded() {
        let store = QuotaStore::open_in_memory().unwrap();
        store.record(Provider::Codex, 10_000, 100).unwrap();
        store.record(Provider::Gemini, 50, 5).unwrap();
        let providers = vec![Provider::Codex, Provider::Gemini];
        assert_eq!(pick_worker_by_provider(&providers, &store).unwrap(), 1);
    }
}
