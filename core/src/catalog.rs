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
    /// A headless token the user exports (`claude setup-token` → env var).
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
/// ORDERING CONVENTION: within each provider, entries are listed best/newest
/// first — `top_model(provider)` returns the first one. When a provider ships a
/// newer top model, add it as that provider's first entry (demoting the one it
/// replaces) AND add the replaced model to `superseded_models`; everything
/// downstream then re-resolves to the new top.
pub fn catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry {
            provider: Provider::Claude,
            model: "claude-opus-4-8".into(),
            auth_method: AuthMethod::SetupToken,
            scores: RoleScores {
                conductor: 10,
                worker: 6,
                reviewer: 8,
                supervisor: 7,
            },
            tier: "frontier",
        },
        CatalogEntry {
            provider: Provider::Claude,
            model: "claude-sonnet-4-6".into(),
            auth_method: AuthMethod::SetupToken,
            scores: RoleScores {
                conductor: 8,
                worker: 7,
                reviewer: 7,
                supervisor: 7,
            },
            tier: "mid",
        },
        CatalogEntry {
            provider: Provider::Codex,
            model: "gpt-5.5".into(),
            auth_method: AuthMethod::CliLogin,
            scores: RoleScores {
                conductor: 7,
                worker: 9,
                reviewer: 9,
                supervisor: 7,
            },
            tier: "frontier",
        },
        CatalogEntry {
            provider: Provider::Gemini,
            model: "Gemini 3.1 Pro (High)".into(),
            auth_method: AuthMethod::CliLogin,
            scores: RoleScores {
                conductor: 7,
                worker: 8,
                reviewer: 7,
                supervisor: 9,
            },
            tier: "frontier",
        },
        // Beta: the Grok Build CLI's headless event schema is unstable (see
        // adapters::grok's doc comment) and its token accounting is unverified
        // against real output — scored modestly below every other provider's
        // corresponding role so it never displaces the curated default lineup
        // (see recommend::tests::full_catalog_reproduces_the_curated_default_lineup),
        // but is still available as an explicit choice once authed.
        CatalogEntry {
            provider: Provider::Grok,
            model: "grok-build".into(),
            auth_method: AuthMethod::CliLogin,
            scores: RoleScores {
                conductor: 5,
                worker: 6,
                reviewer: 5,
                supervisor: 5,
            },
            tier: "beta",
        },
    ]
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

/// Retired models: a specific `(provider, model)` superseded by a newer version
/// of the *same* line. This is deliberately an explicit allowlist, NOT "anything
/// missing from the catalog" — a configured model can be valid yet uncurated
/// (e.g. a cross-family Antigravity fallback: the `gemini` provider running a
/// Claude model), and that must never be mistaken for stale. Add a row when a
/// provider ships a new version; the replacement is always the provider's
/// current `top_model`. Drives the staleness hint and `consilium models`.
pub fn superseded_models() -> Vec<(Provider, &'static str)> {
    vec![(Provider::Codex, "gpt-5.4")]
}

/// Whether `(provider, model)` has been retired in favor of a newer version
/// (see `superseded_models`). `crate::models::stale_models` flags these.
pub fn is_superseded(provider: Provider, model: &str) -> bool {
    superseded_models()
        .iter()
        .any(|(p, m)| *p == provider && *m == model)
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
    fn entries_for_filters_by_provider() {
        let claude = entries_for(Provider::Claude);
        assert!(claude.len() >= 2, "Claude has opus + sonnet");
        assert!(claude.iter().all(|e| e.provider == Provider::Claude));
    }

    #[test]
    fn top_model_is_the_first_entry_per_provider() {
        assert_eq!(
            top_model(Provider::Claude).unwrap().model,
            "claude-opus-4-8"
        );
        assert_eq!(top_model(Provider::Codex).unwrap().model, "gpt-5.5");
        assert!(top_model(Provider::Gemini).is_some());
        assert_eq!(top_model(Provider::Grok).unwrap().model, "grok-build");
    }

    #[test]
    fn is_superseded_flags_only_explicitly_retired_models() {
        // explicitly retired
        assert!(is_superseded(Provider::Codex, "gpt-5.4"));
        // the current top is not retired
        assert!(!is_superseded(Provider::Codex, "gpt-5.5"));
        // a valid-but-uncurated cross-family fallback must NOT be flagged
        assert!(!is_superseded(
            Provider::Gemini,
            "Claude Opus 4.6 (Thinking)"
        ));
    }

    #[test]
    fn superseded_models_have_a_current_top_replacement() {
        // every retired model's provider must still have a top model to point to
        for (provider, _retired) in superseded_models() {
            assert!(
                top_model(provider).is_some(),
                "retired model for {provider:?} has no replacement top_model"
            );
        }
    }
}
