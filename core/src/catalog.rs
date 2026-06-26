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
/// first — `top_model(provider)` returns the first one, and `crate::models`
/// treats any configured model *absent* from this list as superseded. When a
/// provider ships a newer top model, add it as that provider's first entry (and
/// drop or demote the one it replaces); everything downstream re-resolves.
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

/// Whether `(provider, model)` is an exact, currently-endorsed catalog entry.
/// A configured model that returns `false` here has been superseded (see
/// `crate::models::stale_models`).
pub fn contains_model(provider: Provider, model: &str) -> bool {
    catalog()
        .iter()
        .any(|e| e.provider == provider && e.model == model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_the_three_v1_providers() {
        let c = catalog();
        for p in [Provider::Claude, Provider::Codex, Provider::Gemini] {
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
    }

    #[test]
    fn contains_model_is_exact_and_provider_scoped() {
        assert!(contains_model(Provider::Codex, "gpt-5.5"));
        // superseded / unknown models are not in the catalog
        assert!(!contains_model(Provider::Codex, "gpt-5.4"));
        // right model, wrong provider
        assert!(!contains_model(Provider::Gemini, "gpt-5.5"));
    }
}
