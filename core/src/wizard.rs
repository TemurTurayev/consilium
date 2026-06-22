//! Interactive onboarding wizard for `consilium init`: preview the recommended
//! council → auth providers (detect + guide) → write `consilium.config.json`.
//! The pure helpers below are unit-tested; the interactive `run_init_wizard`
//! shell (added next) is not (it drives `dialoguer` + live probes, like
//! `doctor::probe_model`).

use crate::config::Config;
use crate::config::RolesConfig;
use crate::event::Provider;

/// Distinct providers a resolved lineup uses, across every role's primary AND
/// fallback rungs, in stable first-seen order. Used to tell the user which
/// providers a config depends on.
pub fn providers_in(roles: &RolesConfig) -> Vec<Provider> {
    let mut out: Vec<Provider> = Vec::new();
    fn push(p: Provider, out: &mut Vec<Provider>) {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    let singles = [
        &roles.conductor,
        &roles.chairman,
        &roles.reviewer,
        &roles.supervisor,
    ];
    for role in singles {
        for cand in role.ladder() {
            push(cand.provider, &mut out);
        }
    }
    for role in &roles.workers {
        for cand in role.ladder() {
            push(cand.provider, &mut out);
        }
    }
    out
}

/// Assemble a full `Config` from a resolved role lineup; every non-role field
/// keeps its default (verify off, conductor memory on, etc.).
pub fn build_config(roles: RolesConfig) -> Config {
    Config {
        roles,
        ..Config::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;
    use crate::recommend::recommend_roles;

    #[test]
    fn providers_in_lists_distinct_providers_of_the_default_lineup() {
        let roles = recommend_roles(&catalog()).unwrap();
        let ps = providers_in(&roles);
        assert!(ps.contains(&Provider::Claude));
        assert!(ps.contains(&Provider::Codex));
        assert!(ps.contains(&Provider::Gemini));
        let mut sorted = ps.clone();
        sorted.sort_by_key(|p| format!("{p:?}"));
        sorted.dedup();
        assert_eq!(sorted.len(), ps.len(), "no duplicate providers");
    }

    #[test]
    fn providers_in_single_provider_lineup_is_one() {
        let codex: Vec<_> = catalog()
            .into_iter()
            .filter(|e| e.provider == Provider::Codex)
            .collect();
        let roles = recommend_roles(&codex).unwrap();
        assert_eq!(providers_in(&roles), vec![Provider::Codex]);
    }

    #[test]
    fn build_config_keeps_roles_and_default_rest() {
        let roles = recommend_roles(&catalog()).unwrap();
        let cfg = build_config(roles);
        assert_eq!(cfg.roles.conductor.provider, Provider::Claude);
        let def = Config::default();
        assert_eq!(cfg.max_replans, def.max_replans);
        assert_eq!(cfg.cross_family_review, def.cross_family_review);
        assert_eq!(cfg.budget_secs, def.budget_secs);
    }
}
