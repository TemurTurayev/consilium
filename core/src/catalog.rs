//! The curated, in-binary provider catalog: the single source of "our
//! recommendations." Each entry is a (provider, model) with an auth method,
//! per-role recommendation scores (0–10), and a tier hint. Pure data + lookups,
//! no I/O — the recommendation resolver (`crate::recommend`) and the onboarding
//! wizard read it. A later slice makes the scores remotely refreshable; for now
//! they ship in the binary.

use crate::event::Provider;

/// How a provider's CLI is authenticated — consumed by the auth orchestrator
/// (a later slice). v1 providers all log in via their own CLI; API-key is
/// reserved for future OpenAI-compatible providers (GLM/DeepSeek/Kimi).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// The CLI runs its own interactive login (`codex login`, `agy` login).
    CliLogin,
    /// A headless token (`claude setup-token`) kept in the environment or in
    /// `~/.consilium/secrets.env` (see `crate::secrets`).
    SetupToken,
    /// A raw API key stored in `~/.consilium/secrets.env` (future providers).
    ApiKey,
}

/// Per-role recommendation scores, 0 (unsuitable) – 10 (best). `conductor` also
/// scores the chairman (synthesis is the same skill as conducting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleScores {
    pub conductor: u8,
    pub worker: u8,
    pub reviewer: u8,
    pub supervisor: u8,
}

/// One curated (provider, model) recommendation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub provider: Provider,
    pub model: String,
    pub auth_method: AuthMethod,
    pub scores: RoleScores,
    /// Coarse capability/cost hint, e.g. "frontier" or "mid".
    pub tier: &'static str,
}

/// The curated catalog. Scores are tuned so that, with every provider authed,
/// `crate::recommend::recommend_roles(&catalog())` reproduces the curated
/// `Config::default()` lineup (see the recommend module's keystone test).
///
/// Model ids were verified against the live CLIs on 2026-11-17 (claude 2.1.292,
/// codex-cli 0.160.1, agy 1.5.10): `codex debug models --bundled`, `agy models`,
/// `grok models`, plus a real probe wherever the test account's plan allowed.
///
/// ORDERING CONVENTION: within each provider, entries are listed best/newest
/// first — `top_model(provider)` returns the first one, and
/// `crate::models::best_live_model` walks them in order to find the best model a
/// given account can actually run (lower tiers exist because the top one is
/// often plan-gated: e.g. a free ChatGPT plan only gets `gpt-6-luna`). When a
/// provider ships a newer top model, add it as that provider's first entry
/// (demoting the one it replaces) AND add the replaced model to
/// `superseded_models`; everything downstream then re-resolves to the new top.
pub fn catalog() -> Vec<CatalogEntry> {
    vec![
        entry(
            Provider::Claude,
            "claude-opus-5-5",
            AuthMethod::SetupToken,
            (10, 6, 8, 7),
            "frontier",
        ),
        entry(
            Provider::Claude,
            "claude-sonnet-5-5",
            AuthMethod::SetupToken,
            (8, 7, 7, 7),
            "mid",
        ),
        // Codex's workhorse on paid ChatGPT plans (the CLI's own default there).
        entry(
            Provider::Codex,
            "gpt-6.1-sol",
            AuthMethod::CliLogin,
            (7, 9, 9, 7),
            "frontier",
        ),
        // The only current Codex model on free/Go plans. Its worker score ties
        // Gemini's top and beats Claude Sonnet's on purpose: where it is all
        // Codex offers, it should still take a worker seat rather than leave a
        // signed-in subscription idle and push that work onto Claude.
        entry(
            Provider::Codex,
            "gpt-6-luna",
            AuthMethod::CliLogin,
            (5, 8, 6, 6),
            "fast",
        ),
        // Antigravity (`agy`) ids. Argon is Google's planner model, Hydrogen its
        // fast implementation model (both need a paid Google AI plan); Flash is
        // what a free plan gets. Argon tops both the worker and the supervisor
        // score on purpose: one Gemini model for both roles means one preflight
        // probe, and every `agy` call costs ~40s of startup regardless of model.
        entry(
            Provider::Gemini,
            "gemini-4-argon-high",
            AuthMethod::CliLogin,
            (7, 8, 7, 9),
            "frontier",
        ),
        entry(
            Provider::Gemini,
            "gemini-4-hydrogen-high",
            AuthMethod::CliLogin,
            (6, 7, 7, 7),
            "fast",
        ),
        entry(
            Provider::Gemini,
            "gemini-3.8-flash-high",
            AuthMethod::CliLogin,
            (5, 6, 5, 6),
            "mid",
        ),
        // Beta: xAI still calls the Grok Build CLI's headless event schema
        // unstable (see adapters::grok's doc comment) — scored modestly below
        // every other provider's corresponding role so it never displaces the
        // curated default lineup (see
        // recommend::tests::full_catalog_reproduces_the_curated_default_lineup),
        // but is still available as an explicit choice once authed. `grok-4.7`
        // is what `grok models` lists, and has answered and chaired a council
        // end to end (grok 1.0.92, 2026-11-22).
        entry(
            Provider::Grok,
            "grok-4.7",
            AuthMethod::CliLogin,
            (5, 6, 5, 5),
            "beta",
        ),
    ]
}

/// Builds one catalog row; `scores` is (conductor, worker, reviewer, supervisor).
fn entry(
    provider: Provider,
    model: &str,
    auth_method: AuthMethod,
    scores: (u8, u8, u8, u8),
    tier: &'static str,
) -> CatalogEntry {
    let (conductor, worker, reviewer, supervisor) = scores;
    CatalogEntry {
        provider,
        model: model.into(),
        auth_method,
        scores: RoleScores {
            conductor,
            worker,
            reviewer,
            supervisor,
        },
        tier,
    }
}

/// All catalog entries for one provider, best/newest first (catalog order).
pub fn entries_for(provider: Provider) -> Vec<CatalogEntry> {
    catalog()
        .into_iter()
        .filter(|e| e.provider == provider)
        .collect()
}

/// The provider's top (best/newest) catalog model — its first entry — or `None`
/// if the provider has no catalog entry.
pub fn top_model(provider: Provider) -> Option<CatalogEntry> {
    entries_for(provider).into_iter().next()
}

/// A model's position in its provider's best-first list (0 = top), or `None`
/// when it is not a catalog model (uncurated, retired, or the CLI default).
pub fn rank(provider: Provider, model: &str) -> Option<usize> {
    entries_for(provider).iter().position(|e| e.model == model)
}

/// A retired model and the catalog model that replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superseded {
    pub provider: Provider,
    pub retired: &'static str,
    pub successor: &'static str,
}

/// Retired models: a specific `(provider, model)` superseded by a newer version
/// of the *same* line. This is deliberately an explicit allowlist, NOT "anything
/// missing from the catalog" — a configured model can be valid yet uncurated
/// (e.g. a cross-family Antigravity fallback: the `gemini` provider running a
/// Claude model), and that must never be mistaken for stale. Add a row when a
/// provider ships a new version, naming the same-tier catalog model that
/// replaces it. Drives the staleness hint and `consilium models`.
pub fn superseded_models() -> Vec<Superseded> {
    let row = |provider, retired, successor| Superseded {
        provider,
        retired,
        successor,
    };
    vec![
        row(Provider::Claude, "claude-opus-4-8", "claude-opus-5-5"),
        row(Provider::Claude, "claude-sonnet-4-6", "claude-sonnet-5-5"),
        row(Provider::Codex, "gpt-5.4", "gpt-6.1-sol"),
        row(Provider::Codex, "gpt-5.5", "gpt-6.1-sol"),
        row(
            Provider::Gemini,
            "Gemini 3.1 Pro (High)",
            "gemini-4-argon-high",
        ),
        row(Provider::Grok, "grok-build", "grok-4.7"),
    ]
}

/// The catalog model that replaces a retired `(provider, model)`, or `None`
/// when the model has not been retired.
pub fn successor_of(provider: Provider, model: &str) -> Option<&'static str> {
    superseded_models()
        .into_iter()
        .find(|s| s.provider == provider && s.retired == model)
        .map(|s| s.successor)
}

/// Whether `(provider, model)` has been retired in favor of a newer version
/// (see `superseded_models`). `crate::models::stale_models` flags these.
pub fn is_superseded(provider: Provider, model: &str) -> bool {
    successor_of(provider, model).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_all_four_providers() {
        let c = catalog();
        for p in [
            Provider::Claude,
            Provider::Codex,
            Provider::Gemini,
            Provider::Grok,
        ] {
            assert!(
                c.iter().any(|e| e.provider == p),
                "catalog must cover {p:?}"
            );
        }
    }

    #[test]
    fn catalog_entries_are_unique_by_provider_and_model() {
        let c = catalog();
        let mut seen = std::collections::HashSet::new();
        for e in &c {
            assert!(
                seen.insert((e.provider, e.model.clone())),
                "duplicate catalog entry: {:?} {}",
                e.provider,
                e.model
            );
        }
    }

    #[test]
    fn scores_are_in_range() {
        for e in catalog() {
            for s in [
                e.scores.conductor,
                e.scores.worker,
                e.scores.reviewer,
                e.scores.supervisor,
            ] {
                assert!(s <= 10, "score out of range for {}: {s}", e.model);
            }
        }
    }

    #[test]
    fn grok_scores_are_modest_and_never_beat_the_curated_default_lineup() {
        // Grok is beta — its scores must stay below every other provider's best
        // per role so it never silently displaces the curated default lineup
        // (see recommend::tests::full_catalog_reproduces_the_curated_default_lineup).
        let c = catalog();
        let grok = c.iter().find(|e| e.provider == Provider::Grok).unwrap();
        let best_other = |score: fn(&RoleScores) -> u8| -> u8 {
            c.iter()
                .filter(|e| e.provider != Provider::Grok)
                .map(|e| score(&e.scores))
                .max()
                .expect("catalog has non-grok entries")
        };
        assert!(grok.scores.conductor < best_other(|s| s.conductor));
        assert!(grok.scores.worker < best_other(|s| s.worker));
        assert!(grok.scores.reviewer < best_other(|s| s.reviewer));
        assert!(grok.scores.supervisor < best_other(|s| s.supervisor));
    }

    #[test]
    fn claude_uses_setup_token_others_cli_login() {
        let c = catalog();
        let claude = c.iter().find(|e| e.provider == Provider::Claude).unwrap();
        assert_eq!(claude.auth_method, AuthMethod::SetupToken);
        let codex = c.iter().find(|e| e.provider == Provider::Codex).unwrap();
        assert_eq!(codex.auth_method, AuthMethod::CliLogin);
    }

    #[test]
    fn every_provider_lists_its_models_best_first() {
        let models = |p| -> Vec<String> { entries_for(p).into_iter().map(|e| e.model).collect() };
        assert_eq!(
            models(Provider::Claude),
            ["claude-opus-5-5", "claude-sonnet-5-5"]
        );
        // A plan-gated top model is followed by what cheaper plans can run.
        assert_eq!(models(Provider::Codex), ["gpt-6.1-sol", "gpt-6-luna"]);
        assert_eq!(
            models(Provider::Gemini),
            [
                "gemini-4-argon-high",
                "gemini-4-hydrogen-high",
                "gemini-3.8-flash-high"
            ]
        );
        assert_eq!(models(Provider::Grok), ["grok-4.7"]);
    }

    #[test]
    fn rank_is_the_position_in_the_best_first_list() {
        assert_eq!(rank(Provider::Codex, "gpt-6.1-sol"), Some(0));
        assert_eq!(rank(Provider::Codex, "gpt-6-luna"), Some(1));
        // retired, uncurated and CLI-default models are not ranked
        assert_eq!(rank(Provider::Codex, "gpt-5.5"), None);
        assert_eq!(rank(Provider::Gemini, "Claude Opus 4.6 (Thinking)"), None);
        assert_eq!(rank(Provider::Codex, "default"), None);
        // a model only ranks under its own provider
        assert_eq!(rank(Provider::Claude, "gpt-6-luna"), None);
    }

    #[test]
    fn entries_for_filters_by_provider() {
        let claude = entries_for(Provider::Claude);
        assert!(claude.len() >= 2, "Claude has opus + sonnet");
        assert!(claude.iter().all(|e| e.provider == Provider::Claude));
    }

    #[test]
    fn top_model_is_the_first_entry_per_provider() {
        assert_eq!(
            top_model(Provider::Claude).unwrap().model,
            "claude-opus-5-5"
        );
        assert_eq!(top_model(Provider::Codex).unwrap().model, "gpt-6.1-sol");
        assert_eq!(
            top_model(Provider::Gemini).unwrap().model,
            "gemini-4-argon-high"
        );
        assert_eq!(top_model(Provider::Grok).unwrap().model, "grok-4.7");
    }

    #[test]
    fn is_superseded_flags_only_explicitly_retired_models() {
        // explicitly retired
        assert!(is_superseded(Provider::Codex, "gpt-5.4"));
        assert!(is_superseded(Provider::Codex, "gpt-5.5"));
        // the current top is not retired
        assert!(!is_superseded(Provider::Codex, "gpt-6.1-sol"));
        // a valid-but-uncurated cross-family fallback must NOT be flagged
        assert!(!is_superseded(
            Provider::Gemini,
            "Claude Opus 4.6 (Thinking)"
        ));
        // retirement is per provider
        assert!(!is_superseded(Provider::Claude, "gpt-5.5"));
    }

    /// Every model the v0.2.0 defaults shipped with was retired or renamed by
    /// its provider within four months (found 2026-11-17). Each must map to a
    /// same-tier successor so `consilium models --write` can migrate old configs.
    #[test]
    fn every_v0_2_default_model_has_a_successor() {
        let cases = [
            (Provider::Claude, "claude-opus-4-8", "claude-opus-5-5"),
            (Provider::Claude, "claude-sonnet-4-6", "claude-sonnet-5-5"),
            (Provider::Codex, "gpt-5.5", "gpt-6.1-sol"),
            (
                Provider::Gemini,
                "Gemini 3.1 Pro (High)",
                "gemini-4-argon-high",
            ),
            (Provider::Grok, "grok-build", "grok-4.7"),
        ];
        for (provider, retired, successor) in cases {
            assert_eq!(
                successor_of(provider, retired),
                Some(successor),
                "{provider:?}/{retired}"
            );
        }
        assert_eq!(successor_of(Provider::Codex, "gpt-6.1-sol"), None);
    }

    #[test]
    fn successors_are_current_catalog_models_never_retired_themselves() {
        for s in superseded_models() {
            assert!(
                rank(s.provider, s.successor).is_some(),
                "{:?}/{} points at {}, which is not in the catalog",
                s.provider,
                s.retired,
                s.successor
            );
            assert!(
                rank(s.provider, s.retired).is_none(),
                "{:?}/{} is both retired and in the catalog",
                s.provider,
                s.retired
            );
        }
    }
}
