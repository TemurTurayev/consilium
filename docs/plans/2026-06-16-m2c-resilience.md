# Consilium M2c: Resilience (model availability + ladder failover) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prod doesn't fall over when a model dies or hits its limit. Each role gets an ordered *ladder* of model candidates; on a classified failure the engine demotes to the next rung — **loudly** (logged in CLI output and the transcript, never a silent downgrade). `doctor` probes model availability; conduct/auto fail fast if a role has no live model. Motivated by the real incident: Fable 5 was pulled mid-run (404) and the whole conduct run died.

**Architecture:** A new `orchestrator/resilience.rs` holds failure classification, a per-run `ModelHealth` registry of known-dead models, and `run_with_failover` — the single wrapper around `runner::run_to_completion` that walks a role's ladder. Adapters gain `classify_failure(&str) -> FailureKind`, implemented against REAL captured error strings (claude/codex/gemini) and fixture-tested. Orchestrators (council/conduct/auto) take role *ladders* instead of single adapters and call `run_with_failover`.

**Tech Stack:** existing only (tokio, serde, rusqlite, rand, futures). No new dependencies.

**Repo:** `/Users/temur/Desktop/Claude/consilium`, branch `m2c-resilience` off `main`. Baseline: 125 tests green.

## Key decisions (locked — veto at spec review)

| Decision | Choice | Rejected |
|---|---|---|
| Config schema | `RoleConfig` gains `#[serde(default)] fallbacks: Vec<ModelCandidate>` (ModelCandidate = {provider, model}); backward-compatible (old single-model configs parse, empty fallbacks) | Replace `model: String` with `models: Vec<…>` (breaks every existing config + all `role.model` readers) |
| Ladder shape | Per-role ordered list `[primary (provider,model), ...fallbacks]`; cross-provider allowed (conductor opus→sonnet→codex) | Same-provider-only ladder (too rigid); global provider chain (loses per-role intent) |
| Where failover lives | A function `run_with_failover(ladder, …)` wrapping `run_to_completion` — adapter-level wrappers can't retry across a spawn boundary | `FailoverAdapter: Adapter` (build_command can't observe a RunOutcome to decide demotion) |
| Failure classification | `Adapter::classify_failure(&error) -> FailureKind {ModelUnavailable, RateLimited, Transient}`, per-adapter against real strings, fixture-tested | Central regex over all providers (each CLI phrases errors differently — verified) |
| Demotion policy | ModelUnavailable → mark dead in `ModelHealth`, skip on all future rungs, demote. RateLimited → demote (next rung, possibly different provider), do NOT mark permanently dead. Transient → one retry on same rung, then demote. | Wait-and-retry on rate limit (slower than falling to a free provider); infinite retries |
| Loudness | Every demotion records `{from, to, reason}` → transcript `fallbacks[]` + a `↳ fell back: X → Y (reason)` line on stderr | Silent downgrade (corrupts quality invisibly — unacceptable) |
| Detection | `doctor` probes each configured model (tiny "say ok"); conduct/auto resolve each role's ladder to its first healthy rung at start, fail fast if none | Probe-on-every-call (latency/quota); no preflight (waste a long run on a dead primary) |
| `init` setup flow | LAST task, trim-able: probe defaults → write a `consilium.config.json` with ladders for user confirmation | Interactive wizard (scope creep for M2c) |

## Captured real error strings (classifier ground truth, 2026-06-16)

- **claude** model-unavailable: `There's an issue with the selected model (X). It may not exist or you may not have access to it.`
- **codex** model-unavailable: `{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"The 'X' model is not supported when using Codex with a ChatGPT account."}}`
- **gemini** model-unavailable: `code: 404` … `An unexpected critical error occurred:[object Object]`
- Rate-limit strings (best-effort, refine on first real hit): codex `usage limit reached`; claude `rate limit`/`usage limit`; gemini `RESOURCE_EXHAUSTED`/`429`/`quota`.

---

### Task 1: ModelCandidate + RoleConfig.fallbacks + ladder (TDD)

**Files:** Modify `core/src/config.rs`.

- [ ] **Step 1: Failing tests** (add to the config tests module):

```rust
    #[test]
    fn role_without_fallbacks_parses_and_has_single_rung_ladder() {
        let r: RoleConfig = serde_json::from_value(serde_json::json!({
            "provider": "claude", "model": "claude-opus-4-8"
        }))
        .unwrap();
        assert!(r.fallbacks.is_empty());
        let ladder = r.ladder();
        assert_eq!(ladder.len(), 1);
        assert_eq!(ladder[0].provider, Provider::Claude);
        assert_eq!(ladder[0].model, "claude-opus-4-8");
    }

    #[test]
    fn role_with_fallbacks_builds_ordered_ladder() {
        let r: RoleConfig = serde_json::from_value(serde_json::json!({
            "provider": "claude", "model": "claude-opus-4-8",
            "fallbacks": [
                {"provider": "claude", "model": "claude-sonnet-4-6"},
                {"provider": "codex", "model": "gpt-5.4"}
            ]
        }))
        .unwrap();
        let ladder = r.ladder();
        assert_eq!(ladder.len(), 3);
        assert_eq!(ladder[1].model, "claude-sonnet-4-6");
        assert_eq!(ladder[2].provider, Provider::Codex);
    }

    #[test]
    fn default_conductor_has_a_sonnet_fallback() {
        let cfg = Config::default();
        let ladder = cfg.roles.conductor.ladder();
        assert!(ladder.len() >= 2, "conductor should fall back below opus");
        assert_eq!(ladder[0].model, "claude-opus-4-8");
    }
```

- [ ] **Step 2: RED** — `cargo test -p consilium config` → compile error (`ModelCandidate`, `fallbacks`, `ladder` missing).

- [ ] **Step 3: Implement** in config.rs:

```rust
/// One rung of a role's failover ladder: a concrete (provider, model) pair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelCandidate {
    pub provider: Provider,
    pub model: String,
}
```

Add to `RoleConfig` (after `model`):

```rust
    /// Ordered failover candidates tried after the primary (provider, model)
    /// when it is unavailable or rate-limited. Empty = no failover.
    #[serde(default)]
    pub fallbacks: Vec<ModelCandidate>,
```

Add the ladder accessor + keep `new` constructing empty fallbacks:

```rust
impl RoleConfig {
    pub(crate) fn new(provider: Provider, model: &str) -> Self {
        Self {
            provider,
            model: model.into(),
            fallbacks: Vec::new(),
            effort: None,
            mode: None,
            intervention_threshold: None,
        }
    }

    /// Full ordered ladder: primary first, then declared fallbacks.
    pub fn ladder(&self) -> Vec<ModelCandidate> {
        let mut rungs = vec![ModelCandidate {
            provider: self.provider,
            model: self.model.clone(),
        }];
        rungs.extend(self.fallbacks.iter().cloned());
        rungs
    }
}
```

Update the `Default for Config` impl so the two Claude management roles carry a Sonnet fallback (resilience for the next Fable-style event):

```rust
                conductor: RoleConfig {
                    effort: Some("high".into()),
                    mode: Some("attached".into()),
                    fallbacks: vec![ModelCandidate {
                        provider: Provider::Claude,
                        model: "claude-sonnet-4-6".into(),
                    }],
                    ..RoleConfig::new(Provider::Claude, "claude-opus-4-8")
                },
                chairman: RoleConfig {
                    effort: Some("high".into()),
                    fallbacks: vec![ModelCandidate {
                        provider: Provider::Claude,
                        model: "claude-sonnet-4-6".into(),
                    }],
                    ..RoleConfig::new(Provider::Claude, "claude-opus-4-8")
                },
```

(`..RoleConfig::new(...)` already sets `fallbacks: Vec::new()`, so the explicit `fallbacks:` in the struct literal overrides it — verify field-init order compiles; if the struct-update syntax conflicts, set fallbacks via a `let mut` after `new`.)

- [ ] **Step 4: GREEN** — `cargo test -p consilium config` passes (existing config tests unaffected: `parses_spec_example` doesn't set fallbacks → empty). Full suite stays green. fmt + clippy.

- [ ] **Step 5: Commit** `feat: model failover ladder in role config`

### Task 2: FailureKind + per-adapter classify_failure (TDD, real strings)

**Files:** Modify `core/src/adapters/mod.rs` (trait + enum), `core/src/adapters/{claude,codex,gemini}.rs` (impls + tests).

- [ ] **Step 1: Add the enum + trait method** in `core/src/adapters/mod.rs`:

```rust
/// Why a session failed, for failover routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Model does not exist / no access (e.g. pulled like Fable). Permanent —
    /// the model is marked dead for the rest of the run.
    ModelUnavailable,
    /// Provider quota / rate limit hit. Temporary — demote, don't mark dead.
    RateLimited,
    /// Anything else (network, transient). Retry once, then demote.
    Transient,
}
```

Add to the `Adapter` trait (with a default so non-classifying adapters compile):

```rust
    /// Classifies a failure message (from AgentEvent::Failed) for failover.
    /// Default: Transient. Each adapter overrides with patterns matched against
    /// its CLI's REAL error strings (see resilience tests).
    fn classify_failure(&self, error: &str) -> FailureKind {
        let _ = error;
        FailureKind::Transient
    }
```

- [ ] **Step 2: Failing tests** — add to each adapter's test module. Claude (`claude.rs`):

```rust
    #[test]
    fn classifies_model_unavailable() {
        let e = "There's an issue with the selected model (claude-fable-5). It may not exist or you may not have access to it.";
        assert_eq!(ClaudeAdapter.classify_failure(e), FailureKind::ModelUnavailable);
    }

    #[test]
    fn classifies_rate_limit() {
        assert_eq!(
            ClaudeAdapter.classify_failure("Claude usage limit reached; try again later"),
            FailureKind::RateLimited
        );
    }

    #[test]
    fn classifies_other_as_transient() {
        assert_eq!(
            ClaudeAdapter.classify_failure("connection reset by peer"),
            FailureKind::Transient
        );
    }
```

Codex (`codex.rs`):

```rust
    #[test]
    fn classifies_model_unavailable() {
        let e = r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"The 'gpt-bogus' model is not supported when using Codex with a ChatGPT account."}}"#;
        assert_eq!(CodexAdapter.classify_failure(e), FailureKind::ModelUnavailable);
    }

    #[test]
    fn classifies_rate_limit() {
        assert_eq!(
            CodexAdapter.classify_failure("stream error: usage limit reached"),
            FailureKind::RateLimited
        );
    }
```

Gemini (`gemini.rs`):

```rust
    #[test]
    fn classifies_model_unavailable() {
        let e = "  code: 404\nAn unexpected critical error occurred:[object Object]";
        assert_eq!(GeminiAdapter.classify_failure(e), FailureKind::ModelUnavailable);
    }

    #[test]
    fn classifies_rate_limit() {
        assert_eq!(
            GeminiAdapter.classify_failure("Error: RESOURCE_EXHAUSTED (429)"),
            FailureKind::RateLimited
        );
    }
```

- [ ] **Step 3: RED** — `cargo test -p consilium classif` → compile error (FailureKind unused import / method missing).

- [ ] **Step 4: Implement classify_failure per adapter** (case-insensitive substring matching against the captured real strings).

Claude:

```rust
    fn classify_failure(&self, error: &str) -> FailureKind {
        let e = error.to_ascii_lowercase();
        if e.contains("may not exist or you may not have access")
            || e.contains("issue with the selected model")
        {
            FailureKind::ModelUnavailable
        } else if e.contains("rate limit") || e.contains("usage limit") {
            FailureKind::RateLimited
        } else {
            FailureKind::Transient
        }
    }
```

Codex:

```rust
    fn classify_failure(&self, error: &str) -> FailureKind {
        let e = error.to_ascii_lowercase();
        if e.contains("model is not supported") || e.contains("invalid_request_error") {
            FailureKind::ModelUnavailable
        } else if e.contains("usage limit") || e.contains("rate limit") || e.contains("429") {
            FailureKind::RateLimited
        } else {
            FailureKind::Transient
        }
    }
```

Gemini:

```rust
    fn classify_failure(&self, error: &str) -> FailureKind {
        let e = error.to_ascii_lowercase();
        if e.contains("code: 404") || e.contains("not found") || e.contains("critical error") {
            FailureKind::ModelUnavailable
        } else if e.contains("resource_exhausted") || e.contains("429") || e.contains("quota") {
            FailureKind::RateLimited
        } else {
            FailureKind::Transient
        }
    }
```

(Import `FailureKind` into each adapter's test module via `use super::*;` plus the `crate::adapters::FailureKind` path as needed.)

- [ ] **Step 5: GREEN** (≥135 tests), fmt, clippy. **Commit** `feat: per-adapter failure classification from real CLI error strings`

### Task 3: ModelHealth + run_with_failover engine (TDD)

**Files:** Create `core/src/orchestrator/resilience.rs` (+ `pub mod resilience;` in mod.rs, alphabetical — after `prompts`, before `review`... actually after `review`? order: auto, changes, conduct, council, json_extract, prompts, resilience, review, roles, routing, runner, transcript). Create `core/tests/resilience_test.rs`.

- [ ] **Step 1: Failing integration test** `core/tests/resilience_test.rs` (scripted, zero quota):

```rust
mod common;

use common::ScriptedAdapter;
use consilium::adapters::{Adapter, RunRequest};
use consilium::config::ModelCandidate;
use consilium::event::Provider;
use consilium::orchestrator::resilience::{run_with_failover, ModelHealth, Rung};
use consilium::quota::QuotaStore;
use std::sync::Arc;
use std::time::Duration;

fn rung(provider: Provider, model: &str, adapter: Arc<dyn Adapter>) -> Rung {
    Rung {
        candidate: ModelCandidate { provider, model: model.into() },
        adapter,
    }
}

fn req(model: Option<String>) -> RunRequest {
    RunRequest { prompt: "q".into(), model, cwd: std::env::temp_dir(), advisory: true, write: false }
}

#[tokio::test]
async fn first_rung_success_no_fallback() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let ladder = vec![
        rung(Provider::Claude, "opus", Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "done"))),
        rung(Provider::Claude, "sonnet", Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "unused"))),
    ];
    let res = run_with_failover(&ladder, "lbl", |m| req(m), &store, &health, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(res.outcome.final_text, "done");
    assert!(res.fallbacks.is_empty());
    assert_eq!(res.rung_used, 0);
}

#[tokio::test]
async fn model_unavailable_demotes_loudly_and_marks_dead() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let ladder = vec![
        rung(
            Provider::Claude, "claude-fable-5",
            Arc::new(ScriptedAdapter::failing(Provider::Claude, "issue with the selected model (claude-fable-5). It may not exist or you may not have access to it.")),
        ),
        rung(Provider::Claude, "claude-opus-4-8", Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "recovered"))),
    ];
    let res = run_with_failover(&ladder, "conductor", |m| req(m), &store, &health, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(res.outcome.final_text, "recovered");
    assert_eq!(res.rung_used, 1);
    assert_eq!(res.fallbacks.len(), 1);
    assert!(res.fallbacks[0].reason.contains("unavailable"));
    assert!(res.fallbacks[0].from.contains("claude-fable-5"));
    // dead model is remembered
    assert!(health.is_dead(Provider::Claude, "claude-fable-5"));
}

#[tokio::test]
async fn all_rungs_fail_returns_error() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let ladder = vec![
        rung(Provider::Claude, "a", Arc::new(ScriptedAdapter::failing(Provider::Claude, "issue with the selected model (a). may not exist or you may not have access"))),
        rung(Provider::Claude, "b", Arc::new(ScriptedAdapter::failing(Provider::Claude, "issue with the selected model (b). may not exist or you may not have access"))),
    ];
    let err = run_with_failover(&ladder, "conductor", |m| req(m), &store, &health, Duration::from_secs(30))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("all 2 model rungs failed"));
}

#[tokio::test]
async fn dead_rung_is_skipped_on_reuse() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    health.mark_dead(Provider::Claude, "opus");
    let ladder = vec![
        rung(Provider::Claude, "opus", Arc::new(ScriptedAdapter::failing(Provider::Claude, "SHOULD NOT RUN"))),
        rung(Provider::Claude, "sonnet", Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "via-sonnet"))),
    ];
    let res = run_with_failover(&ladder, "x", |m| req(m), &store, &health, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(res.outcome.final_text, "via-sonnet");
    assert_eq!(res.rung_used, 1);
    // skipped-because-dead is still recorded as a fallback for transparency
    assert!(res.fallbacks.iter().any(|f| f.reason.contains("known-dead")));
}
```

- [ ] **Step 2: RED** — `cargo test --test resilience_test` → compile error.

- [ ] **Step 3: Implement** `core/src/orchestrator/resilience.rs`:

```rust
use crate::adapters::{Adapter, FailureKind, RunRequest};
use crate::config::ModelCandidate;
use crate::event::Provider;
use crate::orchestrator::runner::{run_to_completion, RunOutcome, RunStatus};
use crate::quota::QuotaStore;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One ladder rung: a model candidate paired with the adapter that runs it.
pub struct Rung {
    pub candidate: ModelCandidate,
    pub adapter: Arc<dyn Adapter>,
}

/// Per-run registry of models proven dead (ModelUnavailable). Shared across all
/// roles in a run so a model pulled mid-run is skipped everywhere afterward.
#[derive(Clone, Default)]
pub struct ModelHealth {
    dead: Arc<Mutex<HashSet<(Provider, String)>>>,
}

impl ModelHealth {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn mark_dead(&self, provider: Provider, model: &str) {
        self.dead.lock().unwrap().insert((provider, model.to_string()));
    }
    pub fn is_dead(&self, provider: Provider, model: &str) -> bool {
        self.dead.lock().unwrap().contains(&(provider, model.to_string()))
    }
}

/// A single demotion, recorded for the transcript and surfaced on stderr.
#[derive(Debug, Clone)]
pub struct Fallback {
    pub from: String,   // "provider/model"
    pub to: String,     // "provider/model"
    pub reason: String, // human-readable
}

pub struct FailoverResult {
    pub outcome: RunOutcome,
    pub rung_used: usize,
    pub fallbacks: Vec<Fallback>,
    /// The model that ultimately produced the outcome ("provider/model").
    pub model_used: String,
}

fn key(c: &ModelCandidate) -> String {
    format!("{}/{}", c.provider.as_str(), c.model)
}

/// Runs a role's ladder with failover. `build_req` takes the rung's model
/// (Some) and returns the RunRequest for that attempt. Demotes on
/// ModelUnavailable (marks dead) and RateLimited; retries Transient once on the
/// same rung before demoting. Every demotion is recorded in `fallbacks` and
/// logged to stderr. Errors only when every rung is exhausted.
pub async fn run_with_failover(
    ladder: &[Rung],
    label: &str,
    build_req: impl Fn(Option<String>) -> RunRequest,
    quota: &QuotaStore,
    health: &ModelHealth,
    timeout: Duration,
) -> anyhow::Result<FailoverResult> {
    let mut fallbacks: Vec<Fallback> = Vec::new();
    let n = ladder.len();

    for (i, rung) in ladder.iter().enumerate() {
        let model = &rung.candidate.model;
        let provider = rung.candidate.provider;

        // Skip models already known dead this run.
        if health.is_dead(provider, model) {
            if let Some(next) = ladder.get(i + 1) {
                let fb = Fallback {
                    from: key(&rung.candidate),
                    to: key(&next.candidate),
                    reason: format!("{label}: {} is known-dead this run", key(&rung.candidate)),
                };
                eprintln!("↳ {label} fell back: {} → {} ({})", fb.from, fb.to, "known-dead");
                fallbacks.push(fb);
            }
            continue;
        }

        let attempt = |adapter: Arc<dyn Adapter>| {
            let req = build_req(Some(model.clone()));
            run_to_completion(adapter, req, quota, timeout)
        };

        // Transient gets one retry on the same rung before demoting.
        let mut outcome = attempt(rung.adapter.clone()).await?;
        if let RunStatus::Failed(e) = &outcome.status {
            if rung.adapter.classify_failure(e) == FailureKind::Transient {
                outcome = attempt(rung.adapter.clone()).await?;
            }
        }

        // Success on this rung → done.
        if matches!(outcome.status, RunStatus::Completed) {
            return Ok(FailoverResult {
                model_used: key(&rung.candidate),
                outcome,
                rung_used: i,
                fallbacks,
            });
        }

        // Failure → classify, mark dead if permanent, record the demotion.
        let kind = match &outcome.status {
            RunStatus::Failed(e) => rung.adapter.classify_failure(e),
            RunStatus::TimedOut => FailureKind::Transient, // already retried above
            RunStatus::Completed => unreachable!("handled above"),
        };
        if kind == FailureKind::ModelUnavailable {
            health.mark_dead(provider, model);
        }
        let reason = match kind {
            FailureKind::ModelUnavailable => "model unavailable",
            FailureKind::RateLimited => "rate limited",
            FailureKind::Transient => "transient failure",
        };
        if let Some(next) = ladder.get(i + 1) {
            let fb = Fallback {
                from: key(&rung.candidate),
                to: key(&next.candidate),
                reason: format!("{label}: {} ({reason})", key(&rung.candidate)),
            };
            eprintln!("↳ {label} fell back: {} → {} ({reason})", fb.from, fb.to);
            fallbacks.push(fb);
        }
        // Last rung with no successor → loop ends, bail below.
    }

    anyhow::bail!("{label}: all {n} model rungs failed");
}
```

Test note: `all_rungs_fail_returns_error` expects the dead-skip fallback recording too — but on the FINAL rung there is no successor, so no fallback is pushed for it; the two-unavailable-rung test bails after marking both dead (rung 0 records a fallback to rung 1, rung 1 has no successor). The bail message `all 2 model rungs failed` is what the test asserts. The `dead_rung_is_skipped_on_reuse` test pre-marks rung 0 dead, so the loop's `is_dead` branch records the known-dead fallback and continues to rung 1.

- [ ] **Step 4: GREEN** (4 integration tests + suite), fmt, clippy. **Commit** `feat: ModelHealth registry + run_with_failover engine`

### Task 4: Resolve config roles → ladders (TDD)

**Files:** Modify `core/src/orchestrator/roles.rs` — add ladder resolution from a `RoleConfig` to a `Vec<Rung>` (adapter per rung via the existing `adapter_for`-style provider match).

- [ ] **Step 1: Failing test** in roles.rs:

```rust
    #[test]
    fn resolves_role_to_a_rung_per_ladder_entry() {
        let role: RoleConfig = serde_json::from_value(serde_json::json!({
            "provider": "claude", "model": "claude-opus-4-8",
            "fallbacks": [{"provider": "codex", "model": "gpt-5.4"}]
        }))
        .unwrap();
        let ladder = resolve_ladder(&role);
        assert_eq!(ladder.len(), 2);
        assert_eq!(ladder[0].candidate.provider, Provider::Claude);
        assert_eq!(ladder[0].adapter.provider(), Provider::Claude);
        assert_eq!(ladder[1].adapter.provider(), Provider::Codex);
    }
```

- [ ] **Step 2: RED**, then implement in roles.rs:

```rust
use crate::orchestrator::resilience::Rung;

/// Resolves a role config into its failover ladder: one Rung (candidate +
/// adapter) per ladder entry, primary first.
pub fn resolve_ladder(role: &RoleConfig) -> Vec<Rung> {
    role.ladder()
        .into_iter()
        .map(|candidate| {
            let adapter = adapter_for_provider(candidate.provider);
            Rung { candidate, adapter }
        })
        .collect()
}

fn adapter_for_provider(p: Provider) -> std::sync::Arc<dyn Adapter> {
    match p {
        Provider::Claude => std::sync::Arc::new(ClaudeAdapter),
        Provider::Codex => std::sync::Arc::new(CodexAdapter),
        Provider::Gemini => std::sync::Arc::new(GeminiAdapter),
    }
}
```

Refactor the existing `adapter_for(&RoleConfig)` to delegate to `adapter_for_provider(role.provider)` (no duplicate match).

- [ ] **Step 3: GREEN**, fmt, clippy. **Commit** `feat: resolve role configs into failover ladders`

### Task 5: Wire failover into council (TDD)

**Files:** Modify `core/src/orchestrator/council.rs`, `core/tests/council_test.rs`, `core/src/main.rs` (council arm).

- [ ] **Step 1:** Change `CouncilMember` to carry a ladder instead of a single adapter+model:

```rust
pub struct CouncilMember {
    pub label: String,
    pub ladder: Vec<Rung>,
}
```

and `run_council`'s chairman param from `(Arc<dyn Adapter>, Option<String>)` to `chairman_ladder: Vec<Rung>`. Add a `health: &ModelHealth` param. Replace each `run_to_completion(...)` call with `run_with_failover(&ladder, label, |m| RunRequest{...model: m...}, quota, health, timeout)`. Collect `result.fallbacks` into the council transcript under a top-level `"fallbacks"` array, and use `result.outcome` as before.

- [ ] **Step 2:** Update existing council tests: build `CouncilMember { label, ladder: vec![Rung{candidate, adapter}] }` (single-rung) — add a helper `fn solo_member(label, provider, adapter) -> CouncilMember`. Add a new test `council_member_falls_back_to_second_model` (member ladder = [failing-unavailable, ok]) asserting the synthesis still completes and `transcript["fallbacks"]` is non-empty.

- [ ] **Step 3:** main.rs council arm: build members/chairman via `roles::resolve_ladder`, construct a `ModelHealth::new()`, pass through. Print any fallbacks after the synthesis banner.

- [ ] **Step 4: GREEN** (council tests + new one), fmt, clippy. **Commit** `feat: council uses model-failover ladders`

### Task 6: Wire failover into conduct + auto (TDD)

**Files:** Modify `core/src/orchestrator/conduct.rs`, `core/src/orchestrator/auto.rs`, `core/tests/conduct_test.rs`, `core/src/main.rs` (conduct/auto arms).

- [ ] **Step 1:** Change `RoleHandle` to a ladder: `pub struct RoleHandle { pub ladder: Vec<Rung> }` (or replace its `(adapter, model)` with `Vec<Rung>`). `ConductDeps` roles all become ladder-bearing. `run_conduct` takes/creates a `ModelHealth` (add param `health: &ModelHealth`). Replace every `run_to_completion` in conduct (decompose, worker, supervisor, evaluation, arbiter) and `run_review` call with the failover path:
  - decompose/evaluation/supervisor/arbiter: `run_with_failover(&conductor.ladder, "conductor", |m| RunRequest{prompt, model:m, cwd, advisory:true, write:false}, ...)`.
  - worker: `run_with_failover(&worker.ladder, &worker.label, |m| RunRequest{prompt, model:m, cwd, advisory:false, write:true}, ...)` — the infra-failure bail and the decompose status-check stay; failover wraps inside.
  - review: review::run_review currently builds its own request from a single adapter+model; extend it to accept a ladder + health, or have conduct call run_with_failover with a closure that builds the review prompt and parse the verdict from the returned outcome. SIMPLEST: add `review::run_review_ladder(diff, ladder, health, quota, cwd, timeout)` that wraps run_with_failover and parses the verdict; keep the old run_review as a thin wrapper (single-rung ladder) so the M2a review CLI/tests are unchanged.
  - Collect all fallbacks into the conduct transcript `"fallbacks"` array (run-wide).
- [ ] **Step 2:** auto.rs: `AutoDeps` roles become ladders; triage uses the conductor ladder via failover; create one `ModelHealth` for the whole auto run and thread it into both council and conduct so a model that dies during planning is skipped during execution. Compose fallbacks into the auto transcript.
- [ ] **Step 3:** Update all conduct/auto integration tests to build ladders (single-rung helpers). Add `conduct_worker_falls_back` (worker ladder [unavailable, ok-writes-file]) → completed, transcript fallbacks non-empty, file created by the fallback model.
- [ ] **Step 4:** main.rs conduct/auto arms: build all role ladders via `resolve_ladder`, one `ModelHealth::new()` per run, print fallbacks in the summary.
- [ ] **Step 5: GREEN** (full suite), fmt, clippy. **Commit** `feat: conduct and auto use model-failover ladders run-wide`

### Task 7: doctor model probing + conduct/auto preflight (TDD where unit-testable)

**Files:** Modify `core/src/doctor.rs`, `core/src/main.rs`.

- [ ] **Step 1:** Add to doctor.rs a `ModelProbe { provider, model, ok: bool, detail: String }` and `async fn probe_model(adapter, model) -> ModelProbe` that runs a tiny `run_to_completion` with prompt "Reply with: ok" and a short timeout, classifying the result (Completed→ok; Failed→ok=false with the classified reason). Unit-test the CLASSIFICATION mapping with a scripted adapter (ok and unavailable), not real calls.
- [ ] **Step 2:** `doctor` command: after the CLI-presence checks, if `--models` flag is passed, probe every distinct (provider, model) across all configured role ladders and print a ✓/✗ table with the reason. (Real calls — only on `--models`, documented as spending a tiny amount.)
- [ ] **Step 3:** conduct/auto preflight: before the run, for each role resolve the ladder against `ModelHealth` — but do NOT probe every model (that doubles cost). Instead: keep the fail-fast purely structural — if a role's ladder is empty, bail. True liveness is handled by run_with_failover at first use (it already demotes/bails with a clear message). Document this: preflight = config sanity, runtime = failover. (This keeps M2c cheap; a probing preflight can be `--preflight` opt-in reusing probe_model.)
- [ ] **Step 4: GREEN**, fmt, clippy, `cargo run -q -- doctor --help` shows `--models`. **Commit** `feat: doctor --models probes model availability`

### Task 8: `init` setup flow + real E2E smoke + README (sanctioned quota)

**Files:** `core/src/main.rs` (Init command), README.

- [ ] **Step 1:** `consilium init`: probe the default-config models (reuse `doctor::probe_model`), write a `consilium.config.json` to cwd reflecting reachable models as primaries with the unreachable ones demoted/commented, and print what it chose. If `consilium.config.json` already exists, refuse unless `--force`.
- [ ] **Step 2: Real failover smoke (sanctioned):** in a scratch git repo, write a `consilium.config.json` whose conductor primary is a dead model (`claude-fable-5`) with a live fallback (`claude-opus-4-8`), then `consilium conduct "Add a NOTES.md file with one line: hello"`. Expect: a loud `↳ conductor fell back: claude/claude-fable-5 → claude/claude-opus-4-8 (model unavailable)` line, the run completes, NOTES.md created, transcript `fallbacks[]` records the demotion. This proves the whole feature against the exact real incident.
- [ ] **Step 3:** `consilium doctor --models` in that repo shows claude-fable-5 ✗ (unavailable) and the others ✓.
- [ ] **Step 4:** README: new "Resilience" subsection (ladders + loud failover + `doctor --models`); status table M2c ✅. Final gates. **Commit** `feat: M2c resilience complete — model failover verified against a real dead model`

---

## M2c Exit Criteria

- `cargo test` green (≥140 tests), clippy `-D warnings`, fmt clean; test suite spends zero quota.
- A role whose primary model is dead (404) automatically falls to its next rung, the run completes, and the demotion is recorded in the transcript AND printed to stderr — never silent.
- A model proven dead once is skipped for the rest of the run (ModelHealth).
- `doctor --models` reports per-model availability.
- Backward compatible: a config without `fallbacks` behaves exactly as M2b (single-rung ladder, no failover).
- Verified live: conduct with a dead primary + live fallback creates a real file via the fallback model.

## After M2c

**M3:** axum server (REST+WS), MCP attached mode (rmcp) — conductor inside the user's interactive Claude Code session, React web UI, quota-$ dashboards, and the deferred hardening (TimedOut child-kill on write runs, full prompt-injection delimiting, routing pool-exhaustion exclusion).
