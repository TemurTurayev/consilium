//! Top-model resolution: keep the configured lineup on the provider's current
//! best model. The curated `crate::catalog` is the source of truth for which
//! models are endorsed; anything configured *outside* it is treated as
//! superseded. This module provides:
//!
//! - `stale_models` — pure: which configured models have been superseded (free,
//!   no I/O; drives the one-line staleness hint shown before a run).
//! - `best_live_model` / `resolve_top_models` — I/O: probe a provider's catalog
//!   models (best/newest first) and pick the newest one its account can actually
//!   run (handles per-account/tier gating).
//! - `upgrade_config` — pure: rewrite a config so every superseded model adopts
//!   the chosen top model for its provider, leaving intentional (still-endorsed)
//!   choices untouched. Immutable: returns a new `Config`.
//!
//! `consilium models [--write]` wires these together.

use crate::catalog;
use crate::config::{Config, ModelCandidate, RoleConfig, RolesConfig};
use crate::doctor;
use crate::event::Provider;
use crate::quota::QuotaStore;

/// A configured model that has been superseded (is no longer in the catalog),
/// together with the provider's current top model as the suggested replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleModel {
    pub provider: Provider,
    pub current: String,
    /// The provider's top catalog model, or `None` if the provider has no
    /// catalog entry at all (then we can't suggest anything).
    pub suggested: Option<String>,
}

/// Pure: the distinct configured models (across every role's full ladder) that
/// have been explicitly retired in favor of a newer version (see
/// `catalog::superseded_models`). Empty = the lineup is current. No I/O, so this
/// is cheap enough to call before every run for the staleness hint. Valid but
/// uncurated models (e.g. a cross-family Antigravity fallback) are never flagged.
pub fn stale_models(config: &Config) -> Vec<StaleModel> {
    let mut out = Vec::new();
    for candidate in doctor::collect_distinct_model_pairs(config) {
        if catalog::is_superseded(candidate.provider, &candidate.model) {
            out.push(StaleModel {
                provider: candidate.provider,
                current: candidate.model,
                suggested: catalog::top_model(candidate.provider).map(|e| e.model),
            });
        }
    }
    out
}

/// The outcome of resolving one provider's best currently-usable model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopModel {
    /// The newest catalog model that probed live for this account.
    Live(String),
    /// The CLI is present but no catalog model answered (auth/tier/transient).
    NoLiveModel,
    /// The provider's CLI binary is not on PATH.
    CliMissing,
}

/// Probe a provider's catalog models best/newest first and return the first that
/// answers a ~1-token liveness probe — the newest model this account can run.
/// `None` when none answer. I/O (spawns the CLI); not unit-tested.
pub async fn best_live_model(provider: Provider, quota: &QuotaStore) -> Option<ModelCandidate> {
    for entry in catalog::entries_for(provider) {
        let adapter = doctor::adapter_for(provider);
        if doctor::probe_model(adapter, &entry.model, quota).await.ok {
            return Some(ModelCandidate {
                provider,
                model: entry.model,
            });
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

/// Resolve the top live model for every v1 provider concurrently (a cold Claude
/// probe must not serialize the wait). Stable order: claude, codex, gemini.
pub async fn resolve_top_models(quota: &QuotaStore) -> Vec<(Provider, TopModel)> {
    let providers = [Provider::Claude, Provider::Codex, Provider::Gemini];
    let futs = providers
        .into_iter()
        .map(|p| async move { (p, resolve_one(p, quota).await) });
    futures::future::join_all(futs).await
}

/// Resolve a provider's chosen replacement model from `chosen` (a list of
/// (provider, model) the caller probed live).
fn chosen_for(chosen: &[(Provider, String)], provider: Provider) -> Option<&str> {
    chosen
        .iter()
        .find(|(p, _)| *p == provider)
        .map(|(_, m)| m.as_str())
}

/// Pure: the new model string for one (provider, model) — adopt the chosen top
/// model for its provider only if this exact model has been retired; otherwise
/// keep it (still-current or deliberately-uncurated choices are untouched).
fn upgrade_model(provider: Provider, model: &str, chosen: &[(Provider, String)]) -> String {
    if !catalog::is_superseded(provider, model) {
        return model.to_string();
    }
    chosen_for(chosen, provider)
        .map(str::to_string)
        .unwrap_or_else(|| model.to_string())
}

fn upgrade_role(role: &RoleConfig, chosen: &[(Provider, String)]) -> RoleConfig {
    RoleConfig {
        model: upgrade_model(role.provider, &role.model, chosen),
        fallbacks: role
            .fallbacks
            .iter()
            .map(|f| ModelCandidate {
                provider: f.provider,
                model: upgrade_model(f.provider, &f.model, chosen),
            })
            .collect(),
        ..role.clone()
    }
}

/// Pure: a new config where every superseded model (primary or fallback) adopts
/// the chosen top model for its provider. Still-endorsed models — and roles
/// whose provider isn't in `chosen` — are left exactly as they were. Immutable:
/// the input is not modified.
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

    /// A config with a superseded codex model (gpt-5.4) as worker + reviewer and
    /// as the gemini worker's fallback; everything else current.
    fn config_with_stale_codex() -> Config {
        let json = r#"{
          "roles": {
            "conductor":  { "provider": "claude", "model": "claude-opus-4-8",
              "effort": "high", "mode": "attached",
              "fallbacks": [{"provider": "claude", "model": "claude-sonnet-4-6"}] },
            "chairman":   { "provider": "claude", "model": "claude-opus-4-8" },
            "workers":    [
              { "provider": "codex",  "model": "gpt-5.4" },
              { "provider": "gemini", "model": "Gemini 3.1 Pro (High)",
                "fallbacks": [{"provider": "codex", "model": "gpt-5.4"}] }
            ],
            "reviewer":   { "provider": "codex",  "model": "gpt-5.4" },
            "supervisor": { "provider": "gemini", "model": "Gemini 3.1 Pro (High)" }
          }
        }"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn default_config_has_no_stale_models() {
        assert!(stale_models(&Config::default()).is_empty());
    }

    #[test]
    fn detects_superseded_codex_model_and_suggests_top() {
        let stale = stale_models(&config_with_stale_codex());
        // gpt-5.4 appears as worker primary, reviewer primary, and a gemini
        // fallback — but collect_distinct_model_pairs dedupes it to one.
        assert_eq!(stale.len(), 1, "gpt-5.4 should be flagged once: {stale:?}");
        assert_eq!(stale[0].provider, Provider::Codex);
        assert_eq!(stale[0].current, "gpt-5.4");
        assert_eq!(stale[0].suggested.as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn still_endorsed_models_are_not_flagged() {
        // sonnet (a deliberate fallback) is in the catalog → never stale.
        let stale = stale_models(&config_with_stale_codex());
        assert!(stale
            .iter()
            .all(|s| s.current != "claude-sonnet-4-6" && s.current != "claude-opus-4-8"));
    }

    #[test]
    fn upgrade_swaps_only_superseded_models() {
        let cfg = config_with_stale_codex();
        let upgraded = upgrade_config(&cfg, &[(Provider::Codex, "gpt-5.5".to_string())]);

        // codex worker + reviewer + the gemini fallback all move to gpt-5.5
        assert_eq!(upgraded.roles.workers[0].model, "gpt-5.5");
        assert_eq!(upgraded.roles.reviewer.model, "gpt-5.5");
        assert_eq!(upgraded.roles.workers[1].fallbacks[0].model, "gpt-5.5");

        // intentional, still-endorsed choices are untouched
        assert_eq!(upgraded.roles.conductor.model, "claude-opus-4-8");
        assert_eq!(
            upgraded.roles.conductor.fallbacks[0].model,
            "claude-sonnet-4-6"
        );
        assert_eq!(upgraded.roles.workers[1].model, "Gemini 3.1 Pro (High)");

        // and the upgraded config now has nothing stale
        assert!(stale_models(&upgraded).is_empty());
    }

    #[test]
    fn upgrade_is_immutable_and_a_noop_without_a_chosen_model() {
        let cfg = config_with_stale_codex();
        // No chosen model for codex → stale models are left as-is (can't upgrade).
        let same = upgrade_config(&cfg, &[]);
        assert_eq!(same.roles.workers[0].model, "gpt-5.4");
        // original is untouched (immutability)
        assert_eq!(cfg.roles.workers[0].model, "gpt-5.4");
    }

    #[test]
    fn uncurated_cross_family_fallback_is_never_stale_or_rewritten() {
        // The `gemini` provider running a Claude model via Antigravity is a valid,
        // deliberate fallback that isn't in the curated catalog. It must NOT be
        // flagged as stale, and `--write` must leave it exactly as-is — only the
        // explicitly-retired gpt-5.4 moves.
        let json = r#"{
          "roles": {
            "conductor":  { "provider": "claude", "model": "claude-opus-4-8",
              "fallbacks": [{"provider": "gemini", "model": "Claude Opus 4.6 (Thinking)"}] },
            "chairman":   { "provider": "claude", "model": "claude-opus-4-8" },
            "workers":    [ { "provider": "codex", "model": "gpt-5.4" } ],
            "reviewer":   { "provider": "codex", "model": "gpt-5.4" },
            "supervisor": { "provider": "gemini", "model": "Gemini 3.1 Pro (High)" }
          }
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();

        let stale = stale_models(&cfg);
        assert_eq!(stale.len(), 1, "only gpt-5.4 is retired: {stale:?}");
        assert_eq!(stale[0].current, "gpt-5.4");

        let upgraded = upgrade_config(&cfg, &[(Provider::Codex, "gpt-5.5".to_string())]);
        // the cross-family fallback survives untouched
        assert_eq!(
            upgraded.roles.conductor.fallbacks[0].model,
            "Claude Opus 4.6 (Thinking)"
        );
        // and the retired codex model still upgraded
        assert_eq!(upgraded.roles.workers[0].model, "gpt-5.5");
    }

    #[test]
    fn upgrade_preserves_role_hints() {
        let cfg = config_with_stale_codex();
        let upgraded = upgrade_config(&cfg, &[(Provider::Codex, "gpt-5.5".to_string())]);
        // conductor keeps effort/mode; supervisor keeps its provider/model
        assert_eq!(upgraded.roles.conductor.effort.as_deref(), Some("high"));
        assert_eq!(upgraded.roles.conductor.mode.as_deref(), Some("attached"));
    }
}
