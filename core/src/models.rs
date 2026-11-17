//! Top-model resolution: keep the configured lineup on models the account can
//! actually run. The curated `crate::catalog` is the source of truth for which
//! models are endorsed and which have been retired. This module provides:
//!
//! - `stale_models` — pure: which configured models have been retired (free,
//!   no I/O; drives the one-line staleness hint shown before a run).
//! - `best_live_model` / `resolve_top_models` — I/O: probe a provider's catalog
//!   models (best/newest first) and pick the best one its account can actually
//!   run (handles per-account/tier gating), falling back to the CLI's own
//!   default model when the whole catalog has gone stale.
//! - `live_subset` — pure: the slice of the catalog an account can run, which
//!   is what the onboarding wizard builds a lineup from.
//! - `planned_changes` / `upgrade_config` — pure: move every retired or
//!   plan-gated model to one the account can run, leaving intentional
//!   (uncurated) choices untouched. Immutable: returns a new `Config`.
//!
//! `consilium models [--write]` wires these together.

use crate::adapters::{FailureKind, CLI_DEFAULT_MODEL};
use crate::catalog::{self, CatalogEntry};
use crate::config::{Config, ModelCandidate, RoleConfig, RolesConfig};
use crate::doctor;
use crate::event::Provider;
use crate::quota::QuotaStore;

/// A configured model that has been retired, together with the catalog model
/// that replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleModel {
    pub provider: Provider,
    pub current: String,
    /// The same-tier catalog model that replaces it.
    pub suggested: Option<String>,
}

/// Pure: the distinct configured models (across every role's full ladder) that
/// have been explicitly retired in favor of a newer version (see
/// `catalog::superseded_models`). Empty = the lineup is current. No I/O, so this
/// is cheap enough to call before every run for the staleness hint. Valid but
/// uncurated models (e.g. a cross-family Antigravity fallback) are never flagged.
pub fn stale_models(config: &Config) -> Vec<StaleModel> {
    doctor::collect_distinct_model_pairs(config)
        .into_iter()
        .filter_map(|candidate| {
            let successor = catalog::successor_of(candidate.provider, &candidate.model)?;
            Some(StaleModel {
                provider: candidate.provider,
                current: candidate.model,
                suggested: Some(successor.to_string()),
            })
        })
        .collect()
}

/// The outcome of resolving one provider's best currently-usable model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopModel {
    /// The best model that probed live for this account: a catalog model, or
    /// `CLI_DEFAULT_MODEL` when the CLI answers but no catalog model does.
    Live(String),
    /// The CLI is present but nothing answered (auth/transient).
    NoLiveModel,
    /// The provider's CLI binary is not on PATH.
    CliMissing,
}

/// What one probe result means for the best-first search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Walk {
    /// The model answered — this is the account's best live model.
    Found,
    /// The model is unavailable to this account — try the next tier down.
    NextTier,
    /// Anything else (rate limit, timeout, auth, network): this says nothing
    /// about what the plan includes, so stop without an answer rather than
    /// conclude the account is limited to a lower tier.
    Stop,
}

pub(crate) fn walk_step(probe: &doctor::ModelProbe) -> Walk {
    match probe.kind {
        None => Walk::Found,
        Some(FailureKind::ModelUnavailable) => Walk::NextTier,
        Some(FailureKind::RateLimited | FailureKind::Transient) => Walk::Stop,
    }
}

/// Probe a provider's catalog models best/newest first and return the first that
/// answers a ~1-token liveness probe — the best model this account can run. A
/// tier is skipped only when the provider says the model is unavailable; once
/// every catalog id is unavailable, the CLI's own default model is tried (it
/// still works when the whole catalog has been retired or is gated on this
/// plan). `None` when nothing answers or the search was cut short by a
/// temporary failure (see `Walk::Stop`). I/O (spawns the CLI); not unit-tested.
pub async fn best_live_model(provider: Provider, quota: &QuotaStore) -> Option<ModelCandidate> {
    let candidates = catalog::entries_for(provider)
        .into_iter()
        .map(|entry| entry.model)
        .chain(std::iter::once(CLI_DEFAULT_MODEL.to_string()));
    for model in candidates {
        let adapter = doctor::adapter_for(provider);
        match walk_step(&doctor::probe_model(adapter, &model, quota).await) {
            Walk::Found => return Some(ModelCandidate { provider, model }),
            Walk::NextTier => continue,
            Walk::Stop => return None,
        }
    }
    None
}

async fn resolve_one(provider: Provider, quota: &QuotaStore) -> TopModel {
    if !doctor::check(crate::auth::cli_binary(provider)).found {
        return TopModel::CliMissing;
    }
    match best_live_model(provider, quota).await {
        Some(c) => TopModel::Live(c.model),
        None => TopModel::NoLiveModel,
    }
}

/// Resolve the top live model for each given provider concurrently (a cold
/// Claude probe must not serialize the wait). Order follows `providers`.
pub async fn resolve_top_models_for(
    providers: &[Provider],
    quota: &QuotaStore,
) -> Vec<(Provider, TopModel)> {
    let futs = providers
        .iter()
        .map(|&p| async move { (p, resolve_one(p, quota).await) });
    futures::future::join_all(futs).await
}

/// Resolve the top live model for every provider. Stable order: claude, codex,
/// gemini, grok.
pub async fn resolve_top_models(quota: &QuotaStore) -> Vec<(Provider, TopModel)> {
    resolve_top_models_for(
        &[
            Provider::Claude,
            Provider::Codex,
            Provider::Gemini,
            Provider::Grok,
        ],
        quota,
    )
    .await
}

/// The live `(provider, model)` pairs out of a resolution.
pub fn live_models(resolved: &[(Provider, TopModel)]) -> Vec<(Provider, String)> {
    resolved
        .iter()
        .filter_map(|(provider, top)| match top {
            TopModel::Live(model) => Some((*provider, model.clone())),
            TopModel::NoLiveModel | TopModel::CliMissing => None,
        })
        .collect()
}

/// Pure: the part of `catalog` an account can run, given each provider's best
/// live model (`live`, from `live_models`). Per provider: the live model and
/// every tier below it (a plan that runs a tier runs the cheaper ones too);
/// tiers above it are dropped. A provider whose CLI answers only on its own
/// default model keeps one entry — its top entry's role scores, with the model
/// replaced by `CLI_DEFAULT_MODEL`. Providers absent from `live` are dropped.
/// Catalog order is preserved (the recommender's tie-breaks rely on it).
pub fn live_subset(catalog: &[CatalogEntry], live: &[(Provider, String)]) -> Vec<CatalogEntry> {
    catalog
        .iter()
        .filter_map(|entry| {
            let live_model = chosen_for(live, entry.provider)?;
            let entry_rank = catalog::rank(entry.provider, &entry.model)?;
            match catalog::rank(entry.provider, live_model) {
                Some(live_rank) if entry_rank >= live_rank => Some(entry.clone()),
                Some(_) => None,
                None if entry_rank == 0 => Some(CatalogEntry {
                    model: live_model.to_string(),
                    ..entry.clone()
                }),
                None => None,
            }
        })
        .collect()
}

/// Resolve a provider's chosen replacement model from `chosen` (a list of
/// (provider, model) the caller probed live).
fn chosen_for(chosen: &[(Provider, String)], provider: Provider) -> Option<&str> {
    chosen
        .iter()
        .find(|(p, _)| *p == provider)
        .map(|(_, m)| m.as_str())
}

/// Pure: the model a configured `(provider, model)` should move to, given the
/// best model the account can run for that provider (`chosen`).
///
/// - A retired model first resolves to its same-tier successor.
/// - If the account can run that tier (it is at or below the live model in the
///   provider's best-first list), that is the answer — so a retired Sonnet
///   becomes the new Sonnet, not the top Opus.
/// - If the tier is above what the account can run, or only the CLI default
///   answered, the live model is the answer.
/// - Uncurated models (deliberate choices the catalog does not know) and
///   providers with nothing live are left exactly as they were.
fn upgrade_model(provider: Provider, model: &str, chosen: &[(Provider, String)]) -> String {
    let Some(live_top) = chosen_for(chosen, provider) else {
        return model.to_string();
    };
    let target = catalog::successor_of(provider, model).unwrap_or(model);
    let Some(target_rank) = catalog::rank(provider, target) else {
        return model.to_string();
    };
    match catalog::rank(provider, live_top) {
        Some(live_rank) if target_rank >= live_rank => target.to_string(),
        _ => live_top.to_string(),
    }
}

/// Upgrades every rung of a role's ladder, then drops any rung that now
/// repeats an earlier one (two tiers can collapse onto the same live model).
fn upgrade_role(role: &RoleConfig, chosen: &[(Provider, String)]) -> RoleConfig {
    let primary = ModelCandidate {
        provider: role.provider,
        model: upgrade_model(role.provider, &role.model, chosen),
    };
    let fallbacks = role
        .fallbacks
        .iter()
        .map(|f| ModelCandidate {
            provider: f.provider,
            model: upgrade_model(f.provider, &f.model, chosen),
        })
        .fold(Vec::new(), |kept: Vec<ModelCandidate>, rung| {
            if rung == primary || kept.contains(&rung) {
                kept
            } else {
                kept.into_iter().chain(std::iter::once(rung)).collect()
            }
        });
    RoleConfig {
        model: primary.model,
        fallbacks,
        ..role.clone()
    }
}

/// One model swap `upgrade_config` would make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChange {
    pub provider: Provider,
    pub from: String,
    pub to: String,
}

/// Pure: the distinct model swaps `upgrade_config(config, chosen)` would make —
/// what `consilium models` previews before `--write`.
pub fn planned_changes(config: &Config, chosen: &[(Provider, String)]) -> Vec<ModelChange> {
    doctor::collect_distinct_model_pairs(config)
        .into_iter()
        .filter_map(|candidate| {
            let to = upgrade_model(candidate.provider, &candidate.model, chosen);
            (to != candidate.model).then_some(ModelChange {
                provider: candidate.provider,
                from: candidate.model,
                to,
            })
        })
        .collect()
}

/// Pure: a new config where every retired or plan-gated model (primary or
/// fallback) moves to one the account can run (see `upgrade_model`). Uncurated
/// models — and roles whose provider isn't in `chosen` — are left exactly as
/// they were. Immutable: the input is not modified.
pub fn upgrade_config(config: &Config, chosen: &[(Provider, String)]) -> Config {
    let r = &config.roles;
    Config {
        roles: RolesConfig {
            conductor: upgrade_role(&r.conductor, chosen),
            chairman: upgrade_role(&r.chairman, chosen),
            workers: r.workers.iter().map(|w| upgrade_role(w, chosen)).collect(),
            reviewer: upgrade_role(&r.reviewer, chosen),
            supervisor: upgrade_role(&r.supervisor, chosen),
        },
        ..config.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chosen(pairs: &[(Provider, &str)]) -> Vec<(Provider, String)> {
        pairs.iter().map(|(p, m)| (*p, m.to_string())).collect()
    }

    /// The lineup `consilium init` wrote under v0.2.0 — every model in it has
    /// since been retired or renamed by its provider.
    fn v0_2_config() -> Config {
        let json = r#"{
          "roles": {
            "conductor":  { "provider": "claude", "model": "claude-opus-4-8",
              "effort": "high", "mode": "attached",
              "fallbacks": [{"provider": "claude", "model": "claude-sonnet-4-6"}] },
            "chairman":   { "provider": "claude", "model": "claude-opus-4-8" },
            "workers":    [
              { "provider": "codex",  "model": "gpt-5.5" },
              { "provider": "gemini", "model": "Gemini 3.1 Pro (High)",
                "fallbacks": [{"provider": "codex", "model": "gpt-5.5"}] }
            ],
            "reviewer":   { "provider": "codex",  "model": "gpt-5.5" },
            "supervisor": { "provider": "gemini", "model": "Gemini 3.1 Pro (High)" }
          }
        }"#;
        serde_json::from_str(json).unwrap()
    }

    /// Every provider answering on its top catalog model.
    fn top_tier_account() -> Vec<(Provider, String)> {
        chosen(&[
            (Provider::Claude, "claude-opus-5-5"),
            (Provider::Codex, "gpt-6.1-sol"),
            (Provider::Gemini, "gemini-4-argon-high"),
        ])
    }

    #[test]
    fn default_config_has_no_stale_models() {
        assert!(stale_models(&Config::default()).is_empty());
    }

    #[test]
    fn detects_each_retired_model_once_and_suggests_its_successor() {
        let stale = stale_models(&v0_2_config());
        let suggestion = |current: &str| {
            stale
                .iter()
                .find(|s| s.current == current)
                .and_then(|s| s.suggested.clone())
        };
        // gpt-5.5 appears three times in the config but is flagged once
        assert_eq!(stale.len(), 4, "{stale:?}");
        assert_eq!(suggestion("gpt-5.5").as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(
            suggestion("claude-opus-4-8").as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(
            suggestion("claude-sonnet-4-6").as_deref(),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(
            suggestion("Gemini 3.1 Pro (High)").as_deref(),
            Some("gemini-4-argon-high")
        );
    }

    #[test]
    fn upgrade_moves_each_retired_model_to_its_same_tier_successor() {
        let upgraded = upgrade_config(&v0_2_config(), &top_tier_account());

        assert_eq!(upgraded.roles.conductor.model, "claude-opus-5-5");
        // the cheaper fallback stays a cheaper fallback — it must not collapse
        // onto the top model
        assert_eq!(
            upgraded.roles.conductor.fallbacks[0].model,
            "claude-sonnet-5-5"
        );
        assert_eq!(upgraded.roles.workers[0].model, "gpt-6.1-sol");
        assert_eq!(upgraded.roles.workers[1].model, "gemini-4-argon-high");
        assert_eq!(upgraded.roles.workers[1].fallbacks[0].model, "gpt-6.1-sol");
        assert_eq!(upgraded.roles.reviewer.model, "gpt-6.1-sol");
        assert_eq!(upgraded.roles.supervisor.model, "gemini-4-argon-high");

        // and the upgraded config now has nothing stale
        assert!(stale_models(&upgraded).is_empty());
    }

    /// A free ChatGPT plan cannot run `gpt-6.1-sol` (verified 2026-11-17): the
    /// retired model must land on what the account can actually run.
    #[test]
    fn upgrade_respects_a_plan_gated_account() {
        let account = chosen(&[(Provider::Codex, "gpt-6-luna")]);
        let upgraded = upgrade_config(&v0_2_config(), &account);
        assert_eq!(upgraded.roles.workers[0].model, "gpt-6-luna");
        assert_eq!(upgraded.roles.reviewer.model, "gpt-6-luna");
    }

    /// The same holds for a current-but-gated model, e.g. the shipped default
    /// lineup on a free plan.
    #[test]
    fn upgrade_lowers_a_current_model_the_account_cannot_run() {
        let account = chosen(&[(Provider::Codex, "gpt-6-luna")]);
        let upgraded = upgrade_config(&Config::default(), &account);
        assert_eq!(upgraded.roles.workers[0].model, "gpt-6-luna");
        // a provider that was not resolved is left alone
        assert_eq!(
            upgraded.roles.conductor.model,
            Config::default().roles.conductor.model
        );
    }

    #[test]
    fn upgrade_keeps_a_lower_tier_the_account_can_still_run() {
        // An account on the top tier keeps a deliberately cheaper choice.
        let json = r#"{
          "roles": {
            "conductor":  { "provider": "claude", "model": "claude-sonnet-5-5" },
            "chairman":   { "provider": "claude", "model": "claude-sonnet-5-5" },
            "workers":    [ { "provider": "codex", "model": "gpt-6-luna" } ],
            "reviewer":   { "provider": "codex", "model": "gpt-6-luna" },
            "supervisor": { "provider": "gemini", "model": "gemini-3.8-flash-high" }
          }
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(upgrade_config(&cfg, &top_tier_account()), cfg);
    }

    /// When every catalog id for a provider is dead but its CLI still answers
    /// on its own default model, the config adopts `default`.
    #[test]
    fn upgrade_adopts_the_cli_default_when_no_catalog_model_is_live() {
        let account = chosen(&[(Provider::Codex, CLI_DEFAULT_MODEL)]);
        let upgraded = upgrade_config(&v0_2_config(), &account);
        assert_eq!(upgraded.roles.workers[0].model, CLI_DEFAULT_MODEL);
        assert_eq!(upgraded.roles.reviewer.model, CLI_DEFAULT_MODEL);
    }

    fn failed_probe(kind: FailureKind) -> doctor::ModelProbe {
        doctor::ModelProbe {
            provider: Provider::Claude,
            model: "claude-opus-5-5".into(),
            ok: false,
            kind: Some(kind),
            detail: "boom".into(),
        }
    }

    /// A rate limit or a timeout says nothing about what a plan includes. Only
    /// "this model is unavailable" may send the search down a tier — otherwise
    /// one throttled probe would make `models --write` downgrade a healthy
    /// config for good.
    #[test]
    fn only_an_unavailable_model_sends_the_search_down_a_tier() {
        assert_eq!(
            walk_step(&failed_probe(FailureKind::ModelUnavailable)),
            Walk::NextTier
        );
        assert_eq!(
            walk_step(&failed_probe(FailureKind::RateLimited)),
            Walk::Stop
        );
        assert_eq!(walk_step(&failed_probe(FailureKind::Transient)), Walk::Stop);
        let alive = doctor::ModelProbe {
            ok: true,
            kind: None,
            ..failed_probe(FailureKind::Transient)
        };
        assert_eq!(walk_step(&alive), Walk::Found);
    }

    /// An account that only runs Sonnet turns [opus, sonnet, default] into
    /// sonnet twice. The duplicate rung must go: on a transient failure it
    /// would burn a second full retry budget on a rung that just failed.
    #[test]
    fn upgrade_drops_rungs_that_collapse_onto_an_earlier_one() {
        let account = chosen(&[(Provider::Claude, "claude-sonnet-5-5")]);
        let upgraded = upgrade_config(&Config::default(), &account);
        let models: Vec<String> = upgraded
            .roles
            .conductor
            .ladder()
            .into_iter()
            .map(|c| c.model)
            .collect();
        assert_eq!(models, ["claude-sonnet-5-5", CLI_DEFAULT_MODEL]);
    }

    #[test]
    fn upgrade_is_immutable_and_a_noop_without_a_chosen_model() {
        let cfg = v0_2_config();
        // No live model for codex → retired models are left as-is (can't upgrade).
        let same = upgrade_config(&cfg, &[]);
        assert_eq!(same.roles.workers[0].model, "gpt-5.5");
        // original is untouched (immutability)
        assert_eq!(cfg.roles.workers[0].model, "gpt-5.5");
    }

    #[test]
    fn uncurated_and_cli_default_models_are_never_stale_or_rewritten() {
        // The `gemini` provider running a Claude model via Antigravity is a
        // deliberate choice the catalog does not know, and `default` defers to
        // the CLI. Neither may be flagged or rewritten — only gpt-5.4 moves.
        let json = r#"{
          "roles": {
            "conductor":  { "provider": "claude", "model": "default",
              "fallbacks": [{"provider": "gemini", "model": "Claude Opus 4.6 (Thinking)"}] },
            "chairman":   { "provider": "claude", "model": "default" },
            "workers":    [ { "provider": "codex", "model": "gpt-5.4" } ],
            "reviewer":   { "provider": "codex", "model": "gpt-5.4" },
            "supervisor": { "provider": "gemini", "model": "gemini-4-argon-high" }
          }
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();

        let stale = stale_models(&cfg);
        assert_eq!(stale.len(), 1, "only gpt-5.4 is retired: {stale:?}");
        assert_eq!(stale[0].current, "gpt-5.4");

        let upgraded = upgrade_config(&cfg, &top_tier_account());
        assert_eq!(upgraded.roles.conductor.model, "default");
        assert_eq!(
            upgraded.roles.conductor.fallbacks[0].model,
            "Claude Opus 4.6 (Thinking)"
        );
        assert_eq!(upgraded.roles.workers[0].model, "gpt-6.1-sol");
    }

    #[test]
    fn upgrade_preserves_role_hints() {
        let upgraded = upgrade_config(&v0_2_config(), &top_tier_account());
        assert_eq!(upgraded.roles.conductor.effort.as_deref(), Some("high"));
        assert_eq!(upgraded.roles.conductor.mode.as_deref(), Some("attached"));
    }

    #[test]
    fn planned_changes_lists_each_distinct_swap_once() {
        let changes = planned_changes(&v0_2_config(), &top_tier_account());
        let swap = |from: &str| {
            changes
                .iter()
                .find(|c| c.from == from)
                .map(|c| c.to.as_str())
        };
        assert_eq!(changes.len(), 4, "{changes:?}");
        assert_eq!(swap("gpt-5.5"), Some("gpt-6.1-sol"));
        assert_eq!(swap("claude-sonnet-4-6"), Some("claude-sonnet-5-5"));
        // nothing to do for an up-to-date lineup
        assert!(planned_changes(&Config::default(), &top_tier_account()).is_empty());
    }

    #[test]
    fn live_models_keeps_only_providers_that_answered() {
        let resolved = vec![
            (Provider::Claude, TopModel::Live("claude-opus-5-5".into())),
            (Provider::Codex, TopModel::NoLiveModel),
            (Provider::Grok, TopModel::CliMissing),
        ];
        assert_eq!(
            live_models(&resolved),
            chosen(&[(Provider::Claude, "claude-opus-5-5")])
        );
    }

    fn models_of(entries: &[CatalogEntry], provider: Provider) -> Vec<&str> {
        entries
            .iter()
            .filter(|e| e.provider == provider)
            .map(|e| e.model.as_str())
            .collect()
    }

    #[test]
    fn live_subset_drops_tiers_above_what_the_account_can_run() {
        let account = chosen(&[
            (Provider::Claude, "claude-opus-5-5"),
            (Provider::Codex, "gpt-6-luna"),
            (Provider::Gemini, "gemini-4-hydrogen-high"),
        ]);
        let subset = live_subset(&catalog::catalog(), &account);

        assert_eq!(
            models_of(&subset, Provider::Claude),
            ["claude-opus-5-5", "claude-sonnet-5-5"]
        );
        // gpt-6.1-sol is plan-gated for this account → gone
        assert_eq!(models_of(&subset, Provider::Codex), ["gpt-6-luna"]);
        assert_eq!(
            models_of(&subset, Provider::Gemini),
            ["gemini-4-hydrogen-high", "gemini-3.8-flash-high"]
        );
        // grok did not answer → no entry at all
        assert!(models_of(&subset, Provider::Grok).is_empty());
    }

    #[test]
    fn live_subset_keeps_a_provider_that_only_answers_on_its_cli_default() {
        let account = chosen(&[(Provider::Codex, CLI_DEFAULT_MODEL)]);
        let subset = live_subset(&catalog::catalog(), &account);
        assert_eq!(models_of(&subset, Provider::Codex), [CLI_DEFAULT_MODEL]);
        // it keeps the provider's top role scores, so it still wins its roles
        let top = catalog::top_model(Provider::Codex).unwrap();
        assert_eq!(subset[0].scores, top.scores);
    }
}
