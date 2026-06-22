# `consilium init` Onboarding Wizard Implementation Plan (Plan B)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn `consilium init` into the interactive onboarding wizard that wires the catalog + `recommend_roles` + the `auth` orchestrator into one flow: preview the recommended council → auth the providers (detect+guide) → write a working `consilium.config.json`.

**Architecture:** A new `wizard.rs` module with **pure, tested helpers** (`providers_in`, `build_config`) and one **interactive async shell** (`run_init_wizard`) that uses `dialoguer` for menus and consumes `auth::auth_report`/`probe_auth` + `recommend_roles`. `Init` gains `--yes` (non-interactive: write the resolved recommended lineup); a non-TTY stdin auto-falls-back to `--yes` so CI never hangs. The interactive shell is not unit-tested (mirrors `doctor::probe_model` / `auth::probe_auth`); the pure helpers and the `--yes` assembly are.

**Tech Stack:** Rust (edition 2021), crate `consilium` (in `core/`), new dep `dialoguer` (interactive prompts), std `IsTerminal` (TTY detection, no dep), reuses `crate::{catalog, recommend, auth, config, doctor}`.

---

## Status & scope

*Status: DRAFT for review (2026-06-23). Plan B of the auth+wizard design ([docs/specs/2026-06-23-auth-wizard-design.md](../specs/2026-06-23-auth-wizard-design.md)), spec slice 4. Depends on the merged Plan A (`auth.rs`) + the catalog/resolver. This completes the core onboarding (slices 1-4); the model-pool updater (slice 5) is separate.*

**Verification note:** the interactive flow's real proof is an operator-run `consilium init` (needs a TTY + live providers). This plan makes everything *compile + gate-green* and unit-tests the pure parts; the live interactive dogfood (Task 4 Step 3) is run by the operator before merge.

## Decisions carried from the design

- **Detect + guide, live-probe, degrade-not-block** (settled in Plan A, reused here).
- **`dialoguer`** for menus (approved). **`init` becomes the wizard**; `init --yes` writes the recommended lineup non-interactively.
- **v1 simplification:** the auth gate probes **all three v1 providers** (Default and Custom both), then resolves over whatever is `Ready`. (The spec's "auth only the chosen providers" refinement — probe only a Custom pick's providers — is a future tweak; probing 3 once matches the user's accepted "~1 token each" and keeps the flow simple.) Recorded so a reviewer doesn't flag it as a miss.

## File structure

| File | Change | Responsibility |
|------|--------|----------------|
| `core/Cargo.toml` | Modify | add `dialoguer = "0.11"`. |
| `core/src/wizard.rs` | **Create** | pure `providers_in` + `build_config` (tested); interactive `run_init_wizard` (I/O shell). |
| `core/src/lib.rs` | Modify | `pub mod wizard;` |
| `core/src/main.rs` | Modify | `Init { force, yes }`; dispatch wizard vs non-interactive write. |

---

## Tasks

### Task 1: Dependency + pure wizard helpers

**Files:**
- Modify: `core/Cargo.toml`, `core/src/lib.rs`
- Create: `core/src/wizard.rs`

- [ ] **Step 1: Add the dependency.** In `core/Cargo.toml`, under `[dependencies]`, add:

```toml
dialoguer = "0.11"
```

- [ ] **Step 2: Declare the module.** In `core/src/lib.rs`, add (after `pub mod tokenizer;`, keeping it tidy — exact position not load-bearing):

```rust
pub mod wizard;
```

- [ ] **Step 3: Create `core/src/wizard.rs` with the pure helpers + tests** (no interactive code yet):

```rust
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
    let mut push = |p: Provider, out: &mut Vec<Provider>| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    let singles = [&roles.conductor, &roles.chairman, &roles.reviewer, &roles.supervisor];
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
    Config { roles, ..Config::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recommend::recommend_roles;
    use crate::catalog::catalog;

    #[test]
    fn providers_in_lists_distinct_providers_of_the_default_lineup() {
        let roles = recommend_roles(&catalog()).unwrap();
        let ps = providers_in(&roles);
        // The curated lineup spans all three providers.
        assert!(ps.contains(&Provider::Claude));
        assert!(ps.contains(&Provider::Codex));
        assert!(ps.contains(&Provider::Gemini));
        // Distinct — no duplicates.
        let mut sorted = ps.clone();
        sorted.sort_by_key(|p| format!("{p:?}"));
        sorted.dedup();
        assert_eq!(sorted.len(), ps.len());
    }

    #[test]
    fn providers_in_single_provider_lineup_is_one() {
        // Resolve over only Codex → every role is Codex → providers_in == [Codex].
        let codex: Vec<_> = catalog().into_iter().filter(|e| e.provider == Provider::Codex).collect();
        let roles = recommend_roles(&codex).unwrap();
        assert_eq!(providers_in(&roles), vec![Provider::Codex]);
    }

    #[test]
    fn build_config_keeps_roles_and_default_rest() {
        let roles = recommend_roles(&catalog()).unwrap();
        let cfg = build_config(roles);
        // Roles carried through.
        assert_eq!(cfg.roles.conductor.provider, Provider::Claude);
        // Non-role fields match Config::default()'s.
        let def = Config::default();
        assert_eq!(cfg.max_replans, def.max_replans);
        assert_eq!(cfg.cross_family_review, def.cross_family_review);
        assert_eq!(cfg.budget_secs, def.budget_secs);
    }
}
```

- [ ] **Step 4: Run tests — verify they pass.**

Run: `cargo test -p consilium wizard::`
Expected: PASS (3 tests). (`cargo` downloads `dialoguer` on first build — that's fine even though it's unused until Task 2; if clippy flags the unused crate, proceed — Task 2 uses it. If clippy errors hard on the unused dep, move the `dialoguer` line addition to Task 2 Step 1 instead.)

- [ ] **Step 5: Gate + commit** (NO Co-Authored-By trailer):

```bash
cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --check
git add core/Cargo.toml core/Cargo.lock core/src/wizard.rs core/src/lib.rs
git commit -m "feat(wizard): dialoguer dep + pure init-wizard helpers"
```

---

### Task 2: Interactive wizard shell

**Files:**
- Modify: `core/src/wizard.rs` (add the async fn + imports; no new tests — interactive I/O).

- [ ] **Step 1: Extend imports** at the top of `wizard.rs`:

```rust
use crate::auth::{self, ProviderAuth};
use crate::catalog::{catalog, CatalogEntry};
use crate::quota::QuotaStore;
use crate::recommend::recommend_roles;
use dialoguer::{Confirm, Select};
use std::path::Path;
```

- [ ] **Step 2: Add the interactive shell** (after `build_config`, before `#[cfg(test)]`):

```rust
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
    let choice = Select::new()
        .with_prompt("\nHow do you want to set up roles?")
        .items(&["Use the Default (recommended)", "Customize each role"])
        .default(0)
        .interact()?;
    let custom = choice == 1;

    // 4. Auth gate over all v1 providers (concurrent first probe, then guide).
    println!("\nChecking provider auth (this spends ~1 token per provider)…");
    let mut report = auth::auth_report(quota).await;
    loop {
        for (p, status) in &report {
            let mark = if matches!(status, ProviderAuth::Ready) { "✓" } else { "✗" };
            println!("  {mark} {}", auth::guidance(*p, status));
        }
        let not_ready: Vec<Provider> = report
            .iter()
            .filter(|(_, s)| !matches!(s, ProviderAuth::Ready))
            .map(|(p, _)| *p)
            .collect();
        if not_ready.is_empty() {
            break;
        }
        let pick = Select::new()
            .with_prompt("Some providers aren't ready")
            .items(&["Re-check now (after you've logged in)", "Continue with what's ready", "Quit"])
            .default(0)
            .interact()?;
        match pick {
            0 => report = auth::auth_report(quota).await, // re-probe all
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
    let available: Vec<CatalogEntry> =
        catalog().into_iter().filter(|e| ready.contains(&e.provider)).collect();

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
/// (default-highlighting the recommended pick). `available` is non-empty.
fn customize_roles(available: &[CatalogEntry]) -> anyhow::Result<crate::config::RolesConfig> {
    use crate::config::RoleConfig;
    let labels: Vec<String> = available
        .iter()
        .map(|e| format!("{}/{}", e.provider.as_str(), e.model))
        .collect();
    let mut pick_role = |prompt: &str| -> anyhow::Result<RoleConfig> {
        let idx = Select::new().with_prompt(prompt).items(&labels).default(0).interact()?;
        let e = &available[idx];
        Ok(RoleConfig::new(e.provider, &e.model))
    };
    let conductor = pick_role("Conductor (plans + reviews)")?;
    let chairman = pick_role("Chairman (final synthesis)")?;
    let reviewer = pick_role("Reviewer (audits diffs)")?;
    let supervisor = pick_role("Supervisor (watches for trouble)")?;
    let worker = pick_role("Worker (writes the code)")?;
    Ok(crate::config::RolesConfig {
        conductor,
        chairman,
        workers: vec![worker],
        reviewer,
        supervisor,
    })
}

/// Print a one-line-per-role summary of a lineup.
fn print_lineup(roles: &crate::config::RolesConfig) {
    let line = |label: &str, r: &crate::config::RoleConfig| {
        println!("  {label:<11} {}/{}", r.provider.as_str(), r.model);
    };
    line("conductor", &roles.conductor);
    line("chairman", &roles.chairman);
    for (i, w) in roles.workers.iter().enumerate() {
        line(&format!("worker {}", i + 1), w);
    }
    line("reviewer", &roles.reviewer);
    line("supervisor", &roles.supervisor);
}
```

> `RoleConfig::new(provider, &str)` is `pub(crate)` ([config.rs:31](../../core/src/config.rs)) — usable here. The custom path uses single-rung roles (no fallbacks) for simplicity; Default keeps the resolver's ladders.

- [ ] **Step 3: Build + gate** (no new unit tests — interactive shell):

Run: `cargo build -p consilium` then `cargo test -p consilium` && `cargo clippy --all-targets --all-features -- -D warnings` && `cargo fmt --check`.
Expected: builds; all PASS; zero warnings. If `dialoguer`'s `0.11` API differs (e.g. `interact()` signature), adapt the calls minimally to the installed version's docs — the shape (Select with items/default/interact, Confirm with prompt/default/interact) is stable across recent versions.

- [ ] **Step 4: Commit:**

```bash
git add core/src/wizard.rs
git commit -m "feat(wizard): interactive run_init_wizard (preview → auth gate → write)"
```

---

### Task 3: Wire into `consilium init`

**Files:**
- Modify: `core/src/main.rs` (the `Init` variant + handler at ~line 672-684).

- [ ] **Step 1: Add `--yes` to the `Init` variant.** Replace the `Init { force }` variant ([main.rs:82-86](../../core/src/main.rs)) with:

```rust
    /// Set up consilium.config.json. With no flags, runs the interactive
    /// onboarding wizard; --yes writes the recommended lineup non-interactively.
    Init {
        /// Overwrite an existing consilium.config.json without asking.
        #[arg(long)]
        force: bool,
        /// Skip the wizard: write the recommended council non-interactively (CI/scripts).
        #[arg(long)]
        yes: bool,
    },
```

- [ ] **Step 2: Replace the `Command::Init` handler** ([main.rs:672-684](../../core/src/main.rs)) with:

```rust
        Command::Init { force, yes } => {
            use std::io::IsTerminal;
            let target = std::env::current_dir()?.join("consilium.config.json");
            // Non-interactive when --yes, or when stdin isn't a TTY (CI/pipes) —
            // never hang a wizard waiting on input that won't come.
            if yes || !std::io::stdin().is_terminal() {
                if target.exists() && !force {
                    eprintln!("consilium.config.json already exists; use --force to overwrite");
                    std::process::exit(1);
                }
                let roles = consilium::recommend::recommend_roles(&consilium::catalog::catalog())?;
                let cfg = consilium::wizard::build_config(roles);
                std::fs::write(&target, cfg.to_pretty_json()?)?;
                let n_roles = 2 + cfg.roles.workers.len() + 1 + 1;
                println!("wrote consilium.config.json ({n_roles} roles; edit model ladders as needed)");
            } else {
                let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
                consilium::wizard::run_init_wizard(&store, &target, force).await?;
            }
        }
```

- [ ] **Step 3: Build + verify the surface:**

Run: `cargo build -p consilium` && `cargo run -q -p consilium -- init --help`
Expected: builds; help shows `--force` and `--yes`.

- [ ] **Step 4: Verify the non-interactive path writes a valid config** (no tokens — `--yes` doesn't probe):

Run: `cd "$(mktemp -d)" && cargo run -q -p consilium --manifest-path "$OLDPWD/core/Cargo.toml" -- init --yes && head -c 200 consilium.config.json && echo`
Expected: writes `consilium.config.json`; the JSON shows the recommended lineup (conductor claude-opus-4-8, …). (Adjust the manifest-path invocation as needed; the point is to confirm `--yes` writes a parseable config without prompting or probing.)

- [ ] **Step 5: Gate.** `cargo test -p consilium` && `cargo clippy --all-targets --all-features -- -D warnings` && `cargo fmt --check`. Zero warnings.

- [ ] **Step 6: Commit:**

```bash
git add core/src/main.rs
git commit -m "feat(cli): init runs the onboarding wizard (--yes for non-interactive)"
```

---

### Task 4: Docs, operator dogfood & merge

**Files:**
- Modify: `README.md` (roadmap row + a short "Getting started" mention), `/Users/temur/.claude/projects/-Users-temur-Desktop-Claude/memory/project_consilium.md`.

- [ ] **Step 1: Full gate.** `cargo test -p consilium` && `cargo clippy --all-targets --all-features -- -D warnings` && `cargo fmt --check`. All green.

- [ ] **Step 2: README.** Add a roadmap row after "Auth orchestrator":

```
| **Onboarding wizard** | `consilium init` — interactive: preview the recommended council → auth providers (detect + guide) → write `consilium.config.json`; `--yes` for non-interactive | ✅ Done — completes the pick-your-council onboarding |
```

And update the quick-start so `consilium init` is the first step (it now bootstraps auth + config).

- [ ] **Step 3: Operator dogfood (interactive — run by the human, not a subagent).** In a scratch dir: `consilium init`. Confirm: the recommended council prints; the Default/Custom menu works; the auth gate shows ✓/✗ with correct guidance for any un-authed provider; re-check works after logging in; choosing "Continue with what's ready" degrades the lineup; a config is written and parses. Note the observed flow in the merge commit / PR body. **This is the real proof of the interactive UX — do not merge without it.**

- [ ] **Step 4: Memory.** In `project_consilium.md`, mark slice 4 (`consilium init` wizard) done with shas; note the onboarding milestone (slices 1-4) complete, with slice 5 (model-pool updater) as the remaining optional refinement.

- [ ] **Step 5: Commit + merge** per the repo's branch → gate → `merge --no-ff` → push workflow (after the operator dogfood passes).

```bash
git add README.md
git commit -m "docs(wizard): document the consilium init onboarding wizard"
```

---

## Self-review

**1. Spec coverage.** Slice 4 flow — preview recommended (Step 2), Default|Custom (Step 3), auth gate with detect+guide+recheck/skip/quit (Step 4), ≥1-ready requirement (Step 5), `recommend_roles(available)` | custom picks (Step 6), write + verify hint (Step 7), `--yes` non-interactive + non-TTY fallback (Task 3) — all covered. The v1 simplification (auth all 3 vs "only chosen") is explicitly flagged. Overwrite guard + TTY fallback (design error-handling) covered. ✓

**2. Placeholder scan.** No TBD/TODO. The interactive shell is complete code, not a sketch. The dialoguer-version-adapt note (Task 2 Step 3) is a real instruction (the API shape is stable), not a placeholder. ✓

**3. Type consistency.** `providers_in(&RolesConfig) -> Vec<Provider>`, `build_config(RolesConfig) -> Config`, `run_init_wizard(&QuotaStore, &Path, bool)`, `customize_roles(&[CatalogEntry]) -> RolesConfig` consistent across tasks + the main.rs caller. `recommend_roles(&[CatalogEntry]) -> Result<RolesConfig>`, `auth::auth_report(&QuotaStore) -> Vec<(Provider, ProviderAuth)>`, `auth::guidance(Provider, &ProviderAuth)`, `RoleConfig::new`, `RolesConfig { conductor, chairman, workers, reviewer, supervisor }`, `Config { roles, .. }` all match the real merged code (catalog.rs / recommend.rs / auth.rs / config.rs). ✓

**4. Dependency.** One new dep, `dialoguer` (approved). TTY detection via std `IsTerminal` (stable, no dep). Commit `Cargo.lock` so the dep is pinned. ✓
