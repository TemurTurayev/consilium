//! Pure recommendation resolver: given the authed+available subset of the
//! provider catalog, produce a recommended `RolesConfig` — best model per role,
//! resilient ladders, graceful single-provider degradation. Deterministic and
//! exhaustively unit-tested; the onboarding wizard (a later slice) calls this to
//! back its "Default" path. No I/O.

use crate::catalog::{CatalogEntry, RoleScores};
use crate::config::{ModelCandidate, RoleConfig, RolesConfig};
use crate::event::Provider;

/// Resolve the authed+available catalog subset into a recommended role lineup.
///
/// Picks the highest-scoring available model per role (ties keep the earlier
/// catalog entry — deterministic). conductor + chairman get one cheaper fallback
/// (next-best conductor-score model); workers are up to two distinct-provider
/// models by worker score; reviewer/supervisor get the best single model. With a
/// single provider authed, every role resolves to that provider (degraded but
/// functional). Errors only when `available` is empty.
pub fn recommend_roles(available: &[CatalogEntry]) -> anyhow::Result<RolesConfig> {
    if available.is_empty() {
        anyhow::bail!("no authed/available models — authenticate at least one provider first");
    }

    // conductor + chairman: strongest planner, plus one cheaper fallback (the
    // next-best conductor-score model, any provider) for resilience.
    let conductor_primary = best_by(available, |s| s.conductor);
    let conductor_fallback = available
        .iter()
        .filter(|e| !same_model(e, conductor_primary))
        .reduce(|b, e| {
            if e.scores.conductor > b.scores.conductor {
                e
            } else {
                b
            }
        });
    let conductor = role_with_fallback(
        conductor_primary,
        conductor_fallback,
        Some("high"),
        Some("attached"),
    );
    let chairman = role_with_fallback(conductor_primary, conductor_fallback, Some("high"), None);

    // workers: up to two distinct-provider models by worker score (a cross-
    // provider throughput pool; the pool itself is the resilience, so no fallbacks).
    let mut ranked: Vec<&CatalogEntry> = available.iter().collect();
    ranked.sort_by_key(|e| std::cmp::Reverse(e.scores.worker)); // stable: ties keep catalog order
    let mut workers: Vec<RoleConfig> = Vec::new();
    let mut seen: Vec<Provider> = Vec::new();
    for e in ranked {
        if seen.contains(&e.provider) {
            continue;
        }
        seen.push(e.provider);
        workers.push(plain_role(e));
        if workers.len() == 2 {
            break;
        }
    }

    // reviewer + supervisor: best single model for each, no fallback. The
    // supervisor carries the medium intervention-threshold hint (matches default).
    let reviewer = plain_role(best_by(available, |s| s.reviewer));
    let supervisor = {
        let mut r = plain_role(best_by(available, |s| s.supervisor));
        r.intervention_threshold = Some("medium".into());
        r
    };

    Ok(RolesConfig {
        conductor,
        chairman,
        workers,
        reviewer,
        supervisor,
    })
}

fn same_model(a: &CatalogEntry, b: &CatalogEntry) -> bool {
    a.provider == b.provider && a.model == b.model
}

fn to_candidate(e: &CatalogEntry) -> ModelCandidate {
    ModelCandidate {
        provider: e.provider,
        model: e.model.clone(),
    }
}

/// Highest-scoring entry for a role by `score` (strict `>`, so a tie keeps the
/// earlier catalog entry — deterministic). `available` is guaranteed non-empty.
fn best_by(available: &[CatalogEntry], score: impl Fn(&RoleScores) -> u8) -> &CatalogEntry {
    available
        .iter()
        .reduce(|best, e| {
            if score(&e.scores) > score(&best.scores) {
                e
            } else {
                best
            }
        })
        .expect("recommend_roles guarantees a non-empty slice")
}

/// A bare role (primary only, no fallback, no hints).
fn plain_role(e: &CatalogEntry) -> RoleConfig {
    RoleConfig::new(e.provider, &e.model)
}

/// A role with an optional single fallback and optional effort/mode hints.
fn role_with_fallback(
    primary: &CatalogEntry,
    fallback: Option<&CatalogEntry>,
    effort: Option<&str>,
    mode: Option<&str>,
) -> RoleConfig {
    RoleConfig {
        fallbacks: fallback.map(|e| vec![to_candidate(e)]).unwrap_or_default(),
        effort: effort.map(String::from),
        mode: mode.map(String::from),
        ..RoleConfig::new(primary.provider, &primary.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;

    fn find<'a>(c: &'a [CatalogEntry], p: Provider, m: &str) -> &'a CatalogEntry {
        c.iter()
            .find(|e| e.provider == p && e.model == m)
            .expect("entry present")
    }

    #[test]
    fn empty_available_is_an_error() {
        assert!(recommend_roles(&[]).is_err());
    }

    #[test]
    fn full_catalog_reproduces_the_curated_default_lineup() {
        // Keystone: all providers authed → exactly the Config::default() lineup
        // (providers + models per role).
        let roles = recommend_roles(&catalog()).unwrap();

        assert_eq!(roles.conductor.provider, Provider::Claude);
        assert_eq!(roles.conductor.model, "claude-opus-4-8");
        assert_eq!(
            roles.conductor.fallbacks,
            vec![ModelCandidate {
                provider: Provider::Claude,
                model: "claude-sonnet-4-6".into(),
            }]
        );

        assert_eq!(roles.chairman.provider, Provider::Claude);
        assert_eq!(roles.chairman.model, "claude-opus-4-8");

        // workers = two distinct providers, codex then gemini (by worker score).
        let worker_pairs: Vec<(Provider, &str)> = roles
            .workers
            .iter()
            .map(|w| (w.provider, w.model.as_str()))
            .collect();
        assert_eq!(
            worker_pairs,
            vec![
                (Provider::Codex, "gpt-5.5"),
                (Provider::Gemini, "Gemini 3.1 Pro (High)")
            ]
        );

        assert_eq!(roles.reviewer.provider, Provider::Codex);
        assert_eq!(roles.reviewer.model, "gpt-5.5");

        assert_eq!(roles.supervisor.provider, Provider::Gemini);
        assert_eq!(roles.supervisor.model, "Gemini 3.1 Pro (High)");
        assert_eq!(
            roles.supervisor.intervention_threshold.as_deref(),
            Some("medium")
        );
    }

    #[test]
    fn single_provider_fills_every_role_and_never_errors() {
        // Only Codex authed → every role is Codex (degraded, functional).
        let codex = vec![find(&catalog(), Provider::Codex, "gpt-5.5").clone()];
        let roles = recommend_roles(&codex).unwrap();
        for p in [
            roles.conductor.provider,
            roles.chairman.provider,
            roles.reviewer.provider,
            roles.supervisor.provider,
        ] {
            assert_eq!(p, Provider::Codex);
        }
        assert!(!roles.workers.is_empty());
        assert!(roles.workers.iter().all(|w| w.provider == Provider::Codex));
    }

    #[test]
    fn claude_only_prefers_opus_for_conductor_with_sonnet_fallback() {
        // Both Claude models authed → opus conducts, sonnet is the fallback, and
        // the worker pool is Claude's best worker (sonnet > opus on worker score).
        let c = catalog();
        let claude = vec![
            find(&c, Provider::Claude, "claude-opus-4-8").clone(),
            find(&c, Provider::Claude, "claude-sonnet-4-6").clone(),
        ];
        let roles = recommend_roles(&claude).unwrap();
        assert_eq!(roles.conductor.model, "claude-opus-4-8");
        assert_eq!(
            roles.conductor.fallbacks,
            vec![ModelCandidate {
                provider: Provider::Claude,
                model: "claude-sonnet-4-6".into(),
            }]
        );
        // One distinct provider (Claude) → exactly one worker, the best Claude worker.
        assert_eq!(roles.workers.len(), 1);
        assert_eq!(roles.workers[0].model, "claude-sonnet-4-6");
    }

    #[test]
    fn workers_are_distinct_providers() {
        let roles = recommend_roles(&catalog()).unwrap();
        let mut providers: Vec<Provider> = roles.workers.iter().map(|w| w.provider).collect();
        let n = providers.len();
        providers.sort_by_key(|p| format!("{p:?}"));
        providers.dedup();
        assert_eq!(providers.len(), n, "no duplicate-provider workers");
    }
}
