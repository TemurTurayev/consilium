//! Pure recommendation resolver: given the authed+available subset of the
//! provider catalog, produce a recommended `RolesConfig` — best model per role,
//! resilient ladders, graceful single-provider degradation. Deterministic and
//! exhaustively unit-tested; the onboarding wizard (a later slice) calls this to
//! back its "Default" path. No I/O.

use crate::adapters::CLI_DEFAULT_MODEL;
use crate::catalog::{CatalogEntry, RoleScores};
use crate::config::{ModelCandidate, RoleConfig, RolesConfig};
use crate::event::Provider;

/// Resolve the authed+available catalog subset into a recommended role lineup.
///
/// Picks the highest-scoring available model per role (ties keep the earlier
/// catalog entry — deterministic). conductor + chairman get one cheaper fallback
/// (next-best conductor-score model); workers are up to two distinct-provider
/// models by worker score; reviewer/supervisor get the best single model. Every
/// role's ladder then ends on its provider CLI's own default model (see
/// `cli_default_rung`). With a single provider authed, every role resolves to
/// that provider (degraded but functional). Errors only when `available` is empty.
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
    // provider throughput pool; the pool itself is the resilience, so the only
    // fallback is each worker's CLI-default rung).
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

    // reviewer + supervisor: best single model for each. The supervisor
    // carries the medium intervention-threshold hint (matches default).
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

/// The last-resort rung every recommended ladder ends on: the provider CLI's
/// own default model. Named models go stale — providers rename and retire them
/// every few months, and the top ones are often gated by subscription plan — but
/// the CLI's default always exists and always fits the account. So a lineup
/// keeps working (one rung down) when its pinned ids no longer do. `None` when
/// the primary already is the CLI default.
fn cli_default_rung(primary: &CatalogEntry) -> Option<ModelCandidate> {
    (primary.model != CLI_DEFAULT_MODEL).then(|| ModelCandidate {
        provider: primary.provider,
        model: CLI_DEFAULT_MODEL.into(),
    })
}

/// A role with no hints: the primary, then its CLI-default rung.
fn plain_role(e: &CatalogEntry) -> RoleConfig {
    role_with_fallback(e, None, None, None)
}

/// A role with an optional cheaper fallback and optional effort/mode hints. The
/// ladder is: primary → `fallback` (if any) → the primary's CLI-default rung.
fn role_with_fallback(
    primary: &CatalogEntry,
    fallback: Option<&CatalogEntry>,
    effort: Option<&str>,
    mode: Option<&str>,
) -> RoleConfig {
    RoleConfig {
        fallbacks: fallback
            .map(to_candidate)
            .into_iter()
            .chain(cli_default_rung(primary))
            .collect(),
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

    fn rung(provider: Provider, model: &str) -> ModelCandidate {
        ModelCandidate {
            provider,
            model: model.into(),
        }
    }

    #[test]
    fn full_catalog_reproduces_the_curated_default_lineup() {
        // Keystone: all providers authed → exactly the Config::default() lineup.
        let roles = recommend_roles(&catalog()).unwrap();
        assert_eq!(roles, crate::config::Config::default().roles);

        assert_eq!(roles.conductor.provider, Provider::Claude);
        assert_eq!(roles.conductor.model, "claude-opus-5-5");
        assert_eq!(
            roles.conductor.fallbacks,
            vec![
                rung(Provider::Claude, "claude-sonnet-5-5"),
                rung(Provider::Claude, CLI_DEFAULT_MODEL),
            ]
        );

        assert_eq!(roles.chairman.provider, Provider::Claude);
        assert_eq!(roles.chairman.model, "claude-opus-5-5");

        // workers = two distinct providers, codex then gemini (by worker score).
        let worker_pairs: Vec<(Provider, &str)> = roles
            .workers
            .iter()
            .map(|w| (w.provider, w.model.as_str()))
            .collect();
        assert_eq!(
            worker_pairs,
            vec![
                (Provider::Codex, "gpt-6.1-sol"),
                (Provider::Gemini, "gemini-4-argon-high")
            ]
        );

        assert_eq!(roles.reviewer.provider, Provider::Codex);
        assert_eq!(roles.reviewer.model, "gpt-6.1-sol");

        // The supervisor reads a lot and decides — Google's planner model.
        assert_eq!(roles.supervisor.provider, Provider::Gemini);
        assert_eq!(roles.supervisor.model, "gemini-4-argon-high");
        assert_eq!(
            roles.supervisor.intervention_threshold.as_deref(),
            Some("medium")
        );
    }

    /// Regression (2026-11): four months after v0.2.0 every pinned model id had
    /// been retired, renamed or plan-gated, and three of four providers failed
    /// outright. Each recommended ladder must end on its provider CLI's own
    /// default model so a stale id costs one rung, not the whole run.
    #[test]
    fn every_role_ladder_ends_on_its_cli_default() {
        let roles = recommend_roles(&catalog()).unwrap();
        let singles = [
            &roles.conductor,
            &roles.chairman,
            &roles.reviewer,
            &roles.supervisor,
        ];
        for role in singles.into_iter().chain(roles.workers.iter()) {
            assert_eq!(
                role.ladder().last(),
                Some(&rung(role.provider, CLI_DEFAULT_MODEL)),
                "{}/{}",
                role.provider.as_str(),
                role.model
            );
        }
    }

    /// A free ChatGPT plan only runs `gpt-6-luna` (seen live 2026-11-17). Codex
    /// must still earn a worker seat there: leaving a signed-in provider idle
    /// pushes its share of the work onto the Claude subscription instead.
    #[test]
    fn a_plan_limited_codex_still_gets_a_worker_seat() {
        let live = [
            (Provider::Claude, "claude-opus-5-5".to_string()),
            (Provider::Codex, "gpt-6-luna".to_string()),
            (Provider::Gemini, "gemini-4-argon-high".to_string()),
        ];
        let available = crate::models::live_subset(&catalog(), &live);
        let roles = recommend_roles(&available).unwrap();
        let workers: Vec<(Provider, &str)> = roles
            .workers
            .iter()
            .map(|w| (w.provider, w.model.as_str()))
            .collect();
        assert_eq!(
            workers,
            vec![
                (Provider::Codex, "gpt-6-luna"),
                (Provider::Gemini, "gemini-4-argon-high")
            ]
        );
    }

    #[test]
    fn a_cli_default_primary_gets_no_duplicate_default_rung() {
        // What the wizard builds when a provider answers only on its default.
        let c = catalog();
        let codex_default = CatalogEntry {
            model: CLI_DEFAULT_MODEL.into(),
            ..find(&c, Provider::Codex, "gpt-6.1-sol").clone()
        };
        let roles = recommend_roles(&[codex_default]).unwrap();
        assert_eq!(roles.reviewer.model, CLI_DEFAULT_MODEL);
        assert!(roles.reviewer.fallbacks.is_empty());
        assert!(roles.conductor.fallbacks.is_empty());
    }

    #[test]
    fn single_provider_fills_every_role_and_never_errors() {
        // Only Codex authed → every role is Codex (degraded, functional).
        let codex = vec![find(&catalog(), Provider::Codex, "gpt-6.1-sol").clone()];
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
            find(&c, Provider::Claude, "claude-opus-5-5").clone(),
            find(&c, Provider::Claude, "claude-sonnet-5-5").clone(),
        ];
        let roles = recommend_roles(&claude).unwrap();
        assert_eq!(roles.conductor.model, "claude-opus-5-5");
        assert_eq!(
            roles.conductor.fallbacks,
            vec![
                rung(Provider::Claude, "claude-sonnet-5-5"),
                rung(Provider::Claude, CLI_DEFAULT_MODEL),
            ]
        );
        // One distinct provider (Claude) → exactly one worker, the best Claude worker.
        assert_eq!(roles.workers.len(), 1);
        assert_eq!(roles.workers[0].model, "claude-sonnet-5-5");
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
