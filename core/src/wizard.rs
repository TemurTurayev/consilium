//! Interactive onboarding wizard for `consilium init`: preview the recommended
//! council → auth providers (detect + guide) → write `consilium.config.json`.
//! The pure helpers below are unit-tested; the interactive `run_init_wizard`
//! shell (added next) is not (it drives `dialoguer` + live probes, like
//! `doctor::probe_model`).

use crate::auth::{self, ProviderAuth};
use crate::catalog::{catalog, CatalogEntry};
use crate::config::Config;
use crate::config::RolesConfig;
use crate::event::Provider;
use crate::quota::QuotaStore;
use crate::recommend::recommend_roles;
use dialoguer::{Confirm, Select};
use std::path::Path;

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

/// Run the interactive onboarding wizard: preview → choose Default/Custom →
/// auth gate (detect + guide, degrade) → write `consilium.config.json`. Returns
/// Err only on I/O failure or zero ready providers. Not unit-tested (drives
/// `dialoguer` + live probes); proven by an operator dogfood.
pub async fn run_init_wizard(quota: &QuotaStore, target: &Path, force: bool) -> anyhow::Result<()> {
    // 1. Overwrite guard.
    if target.exists() && !force {
        let overwrite = Confirm::new()
            .with_prompt(format!("{} exists — overwrite?", target.display()))
            .default(false)
            .interact()?;
        if !overwrite {
            println!("Keeping the existing config. Nothing written.");
            return Ok(());
        }
    }

    // 2. Preview the recommended council (resolved over the full catalog).
    let recommended = recommend_roles(&catalog())?;
    println!("\nRecommended council (the Default):");
    print_lineup(&recommended);

    // 3. Default or Custom.
    let custom = Select::new()
        .with_prompt("\nHow do you want to set up roles?")
        .items(&["Use the Default (recommended)", "Customize each role"])
        .default(0)
        .interact()?
        == 1;

    // 4. Auth gate over all v1 providers (concurrent first probe, then guide).
    println!("\nChecking provider auth (this spends ~1 token per provider)…");
    let mut report = auth::auth_report(quota).await;
    loop {
        for (p, status) in &report {
            let mark = if matches!(status, ProviderAuth::Ready) {
                "✓"
            } else {
                "✗"
            };
            println!("  {mark} {}", auth::guidance(*p, status));
        }
        let all_ready = report.iter().all(|(_, s)| matches!(s, ProviderAuth::Ready));
        if all_ready {
            break;
        }
        let pick = Select::new()
            .with_prompt("Some providers aren't ready")
            .items(&[
                "Re-check now (after you've logged in)",
                "Continue with what's ready",
                "Quit",
            ])
            .default(0)
            .interact()?;
        match pick {
            0 => report = auth::auth_report(quota).await,
            1 => break,
            _ => anyhow::bail!("onboarding aborted by user"),
        }
    }

    // 5. Require >=1 ready; build the available catalog subset.
    let ready: Vec<Provider> = report
        .iter()
        .filter(|(_, s)| matches!(s, ProviderAuth::Ready))
        .map(|(p, _)| *p)
        .collect();
    if ready.is_empty() {
        anyhow::bail!(
            "no authenticated providers — run the printed login commands (or `consilium auth`) and try again"
        );
    }
    let available: Vec<CatalogEntry> = catalog()
        .into_iter()
        .filter(|e| ready.contains(&e.provider))
        .collect();

    // 6. Resolve roles.
    let roles = if custom {
        customize_roles(&available)?
    } else {
        recommend_roles(&available)?
    };

    // 7. Write config.
    let cfg = build_config(roles);
    std::fs::write(target, cfg.to_pretty_json()?)?;
    println!("\n✓ wrote {}", target.display());
    print_lineup(&cfg.roles);
    println!("\nVerify any time with: consilium doctor --models");
    Ok(())
}

/// Per-role custom picker: for each role, Select a model from `available`
/// (default-highlighting the first). `available` is non-empty.
///
/// Workers use the recommended pool (same two-worker selection as the Default
/// path). Per-worker customization is a future enhancement.
fn customize_roles(available: &[CatalogEntry]) -> anyhow::Result<crate::config::RolesConfig> {
    use crate::config::RoleConfig;
    let labels: Vec<String> = available
        .iter()
        .map(|e| format!("{}/{}", e.provider.as_str(), e.model))
        .collect();
    let pick_role = |prompt: &str| -> anyhow::Result<RoleConfig> {
        let idx = Select::new()
            .with_prompt(prompt)
            .items(&labels)
            .default(0)
            .interact()?;
        let e = &available[idx];
        Ok(RoleConfig::new(e.provider, &e.model))
    };
    let conductor = pick_role("Conductor (plans + reviews)")?;
    let chairman = pick_role("Chairman (final synthesis)")?;
    let reviewer = pick_role("Reviewer (audits diffs)")?;
    let supervisor = pick_role("Supervisor (watches for trouble)")?;
    let workers = recommend_roles(available)?.workers;
    Ok(crate::config::RolesConfig {
        conductor,
        chairman,
        workers,
        reviewer,
        supervisor,
    })
}

/// Print a one-line-per-role summary of a lineup.
fn print_lineup(roles: &crate::config::RolesConfig) {
    fn line(label: &str, r: &crate::config::RoleConfig) {
        println!("  {label:<11} {}/{}", r.provider.as_str(), r.model);
    }
    line("conductor", &roles.conductor);
    line("chairman", &roles.chairman);
    for (i, w) in roles.workers.iter().enumerate() {
        line(&format!("worker {}", i + 1), w);
    }
    line("reviewer", &roles.reviewer);
    line("supervisor", &roles.supervisor);
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
