# Provider Catalog + Recommendation Resolver Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure, in-binary **provider catalog** (providers × models with per-role recommendation scores + auth metadata) and the deterministic **recommendation resolver** (authed+available models → a recommended `RolesConfig`) — the data + logic foundation the onboarding wizard (a later slice) will sit on.

**Architecture:** Two new pure library modules. `catalog.rs` is curated static data — the single source of "our recommendations" — with no I/O. `recommend.rs` is a pure function `recommend_roles(&[CatalogEntry]) -> RolesConfig` that picks the best model per role from whatever is authed+available, builds resilient ladders, and degrades gracefully to a single provider (never "brainless"). Both are exhaustively unit-testable and emit the existing `RolesConfig` type, so the wizard slice can drop the result straight into `Config`. No CLI, no interactivity, no network in this plan.

**Tech Stack:** Rust (edition 2021), crate `consilium` (in `core/`), `serde`/`serde_json`, `anyhow`. Tests are co-located `#[test]` modules. No new dependencies.

---

## Status & scope

*Status: DRAFT for review (2026-06-23). First (foundation) plan of the product-UX onboarding milestone (task #71). Spec: [docs/specs/2026-06-23-product-ux-onboarding-design.md](../specs/2026-06-23-product-ux-onboarding-design.md).*

This plan delivers **spec slices 1 + 2** (the pure foundation):

1. ✅ (this plan) Provider catalog — in-binary providers × models, per-role scores, auth-method metadata.
2. ✅ (this plan) Recommendation resolver — authed+available → default role→model + ladders; deterministic, unit-tested.
3. ⏸ (follow-on plan) Auth orchestrator (`consilium auth`) — per-provider login + `secrets.env` + probe.
4. ⏸ (follow-on plan) Onboarding wizard (`consilium init` interactive) — auth gate → discover → assign → write → verify. **This is what consumes the catalog + resolver.**
5. ⏸ (follow-on plan) Model-pool updater — live discovery + remotely-refreshable catalog + cache.
6. ⏸ (Phase 2, spec) eval-calibrated recommendations.

Slices 3-5 are interactive / network / I/O-heavy and get their own plans (and, for 3+4, their own brainstorm on the auth UX). This plan is intentionally pure so it ships de-risked, exactly like fan-out Phase A.

## Problem (grounded in the spec + the code)

The spec requires onboarding to assign "the best model per role" from a curated set of **recommendations**, constrained to whatever the user has authenticated — with a one-click **Default** path. Today there is no such data or logic: `consilium init` ([main.rs:639-650](../../core/src/main.rs)) writes a hardcoded `Config::default()` ([config.rs:173-212](../../core/src/config.rs)) regardless of which CLIs are authed. The curated lineup is real but **buried in a `Default` impl** — not queryable, not constrainable to authed providers, not scored per role.

This plan extracts that knowledge into a structured, queryable catalog and a resolver that can produce the same lineup *when all providers are available* and a sensible degraded lineup otherwise.

`Config::default()`'s lineup (the target the resolver must reproduce when all three providers are authed):

| Role | Provider / model | Notes |
|------|------------------|-------|
| conductor | Claude `claude-opus-4-8` (fallback `claude-sonnet-4-6`) | effort=high, mode=attached |
| chairman | Claude `claude-opus-4-8` (fallback `claude-sonnet-4-6`) | effort=high |
| workers | Codex `gpt-5.4`, Gemini `Gemini 3.1 Pro (High)` | no fallbacks |
| reviewer | Codex `gpt-5.4` | no fallback |
| supervisor | Gemini `Gemini 3.1 Pro (High)` | interventionThreshold=medium |

## Key design decisions

1. **Two focused modules, both pure.** `catalog.rs` = data + types + lookups; `recommend.rs` = the resolver. They're `pub` library API (no dead-code warnings) and tested directly; the wizard (slice 4) is their first runtime consumer.

2. **Scores are tuned so `recommend_roles(catalog())` reproduces the curated default lineup.** This is the keystone test: feeding the resolver the *full* catalog (all providers authed) yields exactly the per-role providers/models of `Config::default()`. That pins the catalog scores to the owner's actual recommendations and proves the resolver is faithful.

3. **Graceful degradation, never brainless.** With one provider authed, every role resolves to that provider's best-scoring model (functional, just not cross-family). With zero available, the resolver returns `Err` (the wizard's ≥1-auth gate prevents this in practice, but the resolver is honest).

4. **Ladders are minimal and deterministic.** Only conductor + chairman get a single cheaper fallback (next-best conductor-score model) — matching the default. workers/reviewer/supervisor get none (the worker *pool* is the resilience; runtime `cross_family_ladder` already handles cross-family review ordering, so the resolver does not duplicate that). Determinism via stable sort + strict-`>` reductions (ties keep the earlier catalog entry).

5. **`chairman` reuses the `conductor` score.** The spec's role list is conductor/worker/reviewer/supervisor; `RolesConfig` also needs a chairman (synthesis), which is the same skill as conducting — so `RoleScores` has four fields and the resolver maps chairman ← best conductor score.

6. **No CLI wiring here.** `consilium init` still writes `Config::default()` until the wizard slice. This keeps the foundation isolated and reviewable.

## File structure

| File | Change | Responsibility |
|------|--------|----------------|
| `core/src/catalog.rs` | **Create** | `AuthMethod`, `RoleScores`, `CatalogEntry`, `catalog()` static data, `entries_for(provider)` lookup + tests. |
| `core/src/recommend.rs` | **Create** | `recommend_roles(&[CatalogEntry]) -> anyhow::Result<RolesConfig>` + tests. |
| `core/src/lib.rs` | Modify | `pub mod catalog;` `pub mod recommend;` |

---

## Tasks

### Task 1: Provider catalog module

**Files:**
- Create: `core/src/catalog.rs`
- Modify: `core/src/lib.rs`

- [ ] **Step 1: Declare the module.** In `core/src/lib.rs`, add (alphabetical, after `pub mod adapters;` group — place between `pub mod adapters;` and `pub mod config;`):

```rust
pub mod catalog;
```

- [ ] **Step 2: Create `core/src/catalog.rs` with types + data + the test module.** Write the whole file:

```rust
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
pub fn catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry {
            provider: Provider::Claude,
            model: "claude-opus-4-8".into(),
            auth_method: AuthMethod::SetupToken,
            scores: RoleScores { conductor: 10, worker: 6, reviewer: 8, supervisor: 7 },
            tier: "frontier",
        },
        CatalogEntry {
            provider: Provider::Claude,
            model: "claude-sonnet-4-6".into(),
            auth_method: AuthMethod::SetupToken,
            scores: RoleScores { conductor: 8, worker: 7, reviewer: 7, supervisor: 7 },
            tier: "mid",
        },
        CatalogEntry {
            provider: Provider::Codex,
            model: "gpt-5.4".into(),
            auth_method: AuthMethod::CliLogin,
            scores: RoleScores { conductor: 7, worker: 9, reviewer: 9, supervisor: 7 },
            tier: "frontier",
        },
        CatalogEntry {
            provider: Provider::Gemini,
            model: "Gemini 3.1 Pro (High)".into(),
            auth_method: AuthMethod::CliLogin,
            scores: RoleScores { conductor: 7, worker: 8, reviewer: 7, supervisor: 9 },
            tier: "frontier",
        },
    ]
}

/// All catalog entries for one provider, in catalog order.
pub fn entries_for(provider: Provider) -> Vec<CatalogEntry> {
    catalog().into_iter().filter(|e| e.provider == provider).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_the_three_v1_providers() {
        let c = catalog();
        for p in [Provider::Claude, Provider::Codex, Provider::Gemini] {
            assert!(c.iter().any(|e| e.provider == p), "catalog must cover {p:?}");
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
            for s in [e.scores.conductor, e.scores.worker, e.scores.reviewer, e.scores.supervisor] {
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
}
```

- [ ] **Step 3: Run the tests — verify they pass.**

Run: `cargo test -p consilium catalog::`
Expected: PASS (5 tests).

- [ ] **Step 4: Gate + commit** (NO Co-Authored-By trailer — attribution is disabled in this repo):

```bash
cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --check
git add core/src/catalog.rs core/src/lib.rs
git commit -m "feat(catalog): curated provider catalog with per-role recommendation scores"
```

---

### Task 2: Recommendation resolver

**Files:**
- Create: `core/src/recommend.rs`
- Modify: `core/src/lib.rs`

- [ ] **Step 1: Declare the module.** In `core/src/lib.rs`, add (after `pub mod quota;` to keep rough alphabetical order — exact position is not load-bearing, just keep it tidy):

```rust
pub mod recommend;
```

- [ ] **Step 2 (TDD): Create `core/src/recommend.rs` with the resolver signature stub + the full test module FIRST.** Write the file with the function body as `unimplemented!()` so it compiles but tests fail:

```rust
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
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;

    fn find<'a>(c: &'a [CatalogEntry], p: Provider, m: &str) -> &'a CatalogEntry {
        c.iter().find(|e| e.provider == p && e.model == m).expect("entry present")
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
        assert_eq!(roles.conductor.fallbacks, vec![ModelCandidate {
            provider: Provider::Claude, model: "claude-sonnet-4-6".into(),
        }]);

        assert_eq!(roles.chairman.provider, Provider::Claude);
        assert_eq!(roles.chairman.model, "claude-opus-4-8");

        // workers = two distinct providers, codex then gemini (by worker score).
        let worker_pairs: Vec<(Provider, &str)> =
            roles.workers.iter().map(|w| (w.provider, w.model.as_str())).collect();
        assert_eq!(
            worker_pairs,
            vec![(Provider::Codex, "gpt-5.4"), (Provider::Gemini, "Gemini 3.1 Pro (High)")]
        );

        assert_eq!(roles.reviewer.provider, Provider::Codex);
        assert_eq!(roles.reviewer.model, "gpt-5.4");

        assert_eq!(roles.supervisor.provider, Provider::Gemini);
        assert_eq!(roles.supervisor.model, "Gemini 3.1 Pro (High)");
        assert_eq!(roles.supervisor.intervention_threshold.as_deref(), Some("medium"));
    }

    #[test]
    fn single_provider_fills_every_role_and_never_errors() {
        // Only Codex authed → every role is Codex (degraded, functional).
        let codex = vec![find(&catalog(), Provider::Codex, "gpt-5.4").clone()];
        let roles = recommend_roles(&codex).unwrap();
        for p in [
            roles.conductor.provider, roles.chairman.provider,
            roles.reviewer.provider, roles.supervisor.provider,
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
        assert_eq!(roles.conductor.fallbacks, vec![ModelCandidate {
            provider: Provider::Claude, model: "claude-sonnet-4-6".into(),
        }]);
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
```

- [ ] **Step 3: Run the tests — verify they fail** (runtime `not implemented`):

Run: `cargo test -p consilium recommend::`
Expected: FAIL (panics in each test).

- [ ] **Step 4: Implement `recommend_roles`** — replace the `unimplemented!()` body and add the private helpers below it:

```rust
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
        .reduce(|b, e| if e.scores.conductor > b.scores.conductor { e } else { b });
    let conductor = role_with_fallback(conductor_primary, conductor_fallback, Some("high"), Some("attached"));
    let chairman = role_with_fallback(conductor_primary, conductor_fallback, Some("high"), None);

    // workers: up to two distinct-provider models by worker score (a cross-
    // provider throughput pool; the pool itself is the resilience, so no fallbacks).
    let mut ranked: Vec<&CatalogEntry> = available.iter().collect();
    ranked.sort_by(|a, b| b.scores.worker.cmp(&a.scores.worker)); // stable: ties keep catalog order
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

    Ok(RolesConfig { conductor, chairman, workers, reviewer, supervisor })
}

fn same_model(a: &CatalogEntry, b: &CatalogEntry) -> bool {
    a.provider == b.provider && a.model == b.model
}

fn to_candidate(e: &CatalogEntry) -> ModelCandidate {
    ModelCandidate { provider: e.provider, model: e.model.clone() }
}

/// Highest-scoring entry for a role by `score` (strict `>`, so a tie keeps the
/// earlier catalog entry — deterministic). `available` is guaranteed non-empty.
fn best_by(available: &[CatalogEntry], score: impl Fn(&RoleScores) -> u8) -> &CatalogEntry {
    available
        .iter()
        .reduce(|best, e| if score(&e.scores) > score(&best.scores) { e } else { best })
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
```

Note: `RoleConfig::new` is `pub(crate)` ([config.rs:31](../../core/src/config.rs)) — accessible from this in-crate module. `RoleConfig`'s struct-update `..RoleConfig::new(...)` fills `intervention_threshold: None` etc., which the supervisor branch then overrides.

- [ ] **Step 5: Run the tests — verify all pass.**

Run: `cargo test -p consilium recommend::`
Expected: PASS (5 tests). If `full_catalog_reproduces_the_curated_default_lineup` fails, the catalog scores in Task 1 are mistuned — fix the scores, not the test (the test encodes the owner's intended lineup).

- [ ] **Step 6: Gate + commit** (NO Co-Authored-By trailer):

```bash
cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --check
git add core/src/recommend.rs core/src/lib.rs
git commit -m "feat(recommend): deterministic role-recommendation resolver over the catalog"
```

---

### Task 3: Gate, docs & memory

**Files:**
- Modify: `README.md` (one line in the roadmap table), `/Users/temur/.claude/projects/-Users-temur-Desktop-Claude/memory/project_consilium.md`.

- [ ] **Step 1: Full gate (whole crate).**

Run:
```bash
cargo test -p consilium
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```
Expected: all PASS, zero warnings.

- [ ] **Step 2: README roadmap row.** Add one row to the Status/Roadmap table (after the Fan-out DAG row), noting the onboarding foundation:

```
| **Onboarding foundation** | curated provider **catalog** (per-role recommendation scores + auth metadata) + a pure **recommendation resolver** (authed+available → best-model-per-role `RolesConfig`, graceful single-provider degradation) | ✅ Done — `consilium init` wiring + auth wizard are follow-on slices |
```

- [ ] **Step 3: Memory.** In `project_consilium.md`, mark product-UX slices 1+2 (catalog + resolver) done with the commit shas, and note slices 3-5 (auth orchestrator, init wizard, model-pool updater) as the remaining onboarding work (the wizard is the first consumer of these two modules).

- [ ] **Step 4: Commit** (NO Co-Authored-By trailer):

```bash
git add README.md
git commit -m "docs: document the onboarding catalog + recommendation resolver"
```

- [ ] **Step 5: Merge** per the repo's branch → gate → `merge --no-ff` → push workflow.

---

## Follow-on (not in this plan)

- **Slice 3 — Auth orchestrator (`consilium auth`):** per-provider login (consume `AuthMethod`: `claude setup-token`→env, `codex login`, `agy` login, future API-key→`~/.consilium/secrets.env`) + probe verification. Needs a brainstorm on the auth UX (CLI flow shape, where secrets live, how "authed" is probed).
- **Slice 4 — Onboarding wizard (`consilium init` interactive):** the consumer — auth gate (≥1) → live model discovery (`agy models`, etc.) → intersect with `catalog()` → `recommend_roles(available)` for the Default path (or custom per-role pick) → write `consilium.config.json` → `doctor --models` verify. This is where `recommend_roles` finally runs in production.
- **Slice 5 — Model-pool updater:** live CLI discovery + remotely-refreshable recommendations catalog (versioned JSON in the repo, cached in `~/.consilium`, in-binary fallback) so the scores in `catalog.rs` can refresh without a release.

## Self-review

**1. Spec coverage.** Slice 1 (catalog: providers × models, auth-method, per-role scores, tier) → Task 1. Slice 2 (resolver: authed+available → role assignment + ladders, deterministic, degrades to one provider, errors on empty) → Task 2. Slices 3-6 explicitly deferred with a follow-on section (not dropped). ✓

**2. Placeholder scan.** No TBD/TODO; every code step is complete (full module bodies, full test bodies, real commands + expected output). The `unimplemented!()` in Task 2 Step 2 is the deliberate TDD red stub, replaced in Step 4. ✓

**3. Type consistency.** `CatalogEntry`/`RoleScores`/`AuthMethod` defined in Task 1 are used with identical field names in Task 2 (`scores.conductor`, `scores.worker`, etc.). `recommend_roles(&[CatalogEntry]) -> anyhow::Result<RolesConfig>` is consistent between the stub (Step 2) and impl (Step 4). `RoleConfig`/`RolesConfig`/`ModelCandidate` match the real [config.rs](../../core/src/config.rs) definitions (verified: `RolesConfig { conductor, chairman, workers, reviewer, supervisor }`; `RoleConfig` fields `provider/model/fallbacks/effort/mode/intervention_threshold`; `RoleConfig::new` is `pub(crate)`). The keystone test asserts against `Config::default()`'s actual lineup. ✓

**4. Determinism check.** `best_by` uses strict `>` (first max wins); the workers `sort_by` is stable (ties keep catalog order). Both make the resolver output a pure function of catalog order — no `HashMap` iteration or `Math.random`-style nondeterminism. ✓
