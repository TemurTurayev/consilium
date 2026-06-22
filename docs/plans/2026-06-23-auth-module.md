# Auth Module + `consilium auth` Implementation Plan (Plan A)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the **auth orchestrator** — a module that reports each provider's auth state (via a live probe) and prints the exact "detect + guide" next step — plus a `consilium auth` command that runs it.

**Architecture:** A new pure-core + thin-I/O module `auth.rs`, mirroring `doctor.rs`'s split. Pure helpers (`classify`, `is_auth_failure`, `login_command`, `guidance`) are exhaustively unit-tested; the I/O shell (`probe_auth`, `auth_report`) reuses `doctor`'s existing CLI-presence check + model probe and is not unit-tested (it spawns real CLIs). A `consilium auth [--provider P]` command probes providers concurrently and prints status + guidance. No new dependencies.

**Tech Stack:** Rust (edition 2021), crate `consilium` (in `core/`), `anyhow`, `futures` (already a dep), reuses `crate::doctor` + `crate::catalog`. Tests are co-located `#[test]`.

---

## Status & scope

*Status: DRAFT for review (2026-06-23). Plan A of the auth+wizard design ([docs/specs/2026-06-23-auth-wizard-design.md](../specs/2026-06-23-auth-wizard-design.md)), spec slice 3. Plan B (the interactive `consilium init` wizard, slice 4) is a separate plan that depends on this one.*

This plan ships a usable `consilium auth` and the `ProviderAuth` API the wizard (Plan B) will consume. Settled by the design: **detect + guide** (never drive login), **live probe** to confirm readiness, **degrade not block**.

## Problem (grounded in the code)

There is no way to ask "is provider X authenticated, and if not, what do I run?" `doctor.rs` has the raw pieces — `check(binary)` (CLI presence via `--version`), `probe_model(adapter, model, quota)` (a real "reply ok" liveness probe, ~1 token), `adapter_for(provider)`, and `remediation_hint(detail)` (string-matches auth failures) — but nothing composes them per-provider into an actionable auth status. This module does exactly that, then the wizard (Plan B) drives onboarding with it.

`FailureKind` ([adapters/mod.rs:38](../../core/src/adapters/mod.rs)) is coarse (`ModelUnavailable`/`RateLimited`/`Transient`) and does not single out auth/401, so — like `doctor::remediation_hint` — we classify auth-vs-other by **matching the probe's error detail string**, in a pure tested helper.

## File structure

| File | Change | Responsibility |
|------|--------|----------------|
| `core/src/auth.rs` | **Create** | `ProviderAuth`, pure classifiers (`is_auth_failure`, `classify`, `login_command`, `guidance`, `cli_binary`, `primary_model`), I/O shell (`probe_auth`, `auth_report`). |
| `core/src/lib.rs` | Modify | `pub mod auth;` |
| `core/src/main.rs` | Modify | `Auth { provider: Option<String> }` subcommand + handler. |

---

## Tasks

### Task 1: Pure auth-status core

**Files:**
- Create: `core/src/auth.rs`
- Modify: `core/src/lib.rs`

- [ ] **Step 1: Declare the module.** In `core/src/lib.rs`, add (after `pub mod adapters;`, before `pub mod catalog;`):

```rust
pub mod auth;
```

- [ ] **Step 2: Create `core/src/auth.rs` with the pure core + its test module** (no I/O functions yet):

```rust
//! Auth orchestrator: report each provider's auth state and the exact "detect +
//! guide" next step. Pure classifiers (this file's top half) are unit-tested;
//! the I/O shell (`probe_auth`/`auth_report`, added next) reuses `crate::doctor`
//! and is not unit-tested, mirroring `doctor::probe_model`.

use crate::event::Provider;

/// One provider's authentication state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAuth {
    /// The liveness probe succeeded — the provider answers.
    Ready,
    /// CLI is present but the probe failed in an auth-shaped way (carries the detail).
    NeedsLogin(String),
    /// The CLI binary is not on PATH.
    CliMissing,
    /// Present, but the probe failed for a non-auth reason (rate limit, transient…).
    Down(String),
}

/// The CLI binary name for a provider (matches `doctor::run_doctor`'s list).
pub fn cli_binary(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
        Provider::Gemini => "agy",
    }
}

/// True when a probe failure detail looks like an auth/credential problem (vs a
/// transient/other failure). Mirrors `doctor::remediation_hint`'s matching.
pub fn is_auth_failure(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    d.contains("401")
        || d.contains("authenticat")
        || d.contains("unauthor")
        || d.contains("credential")
        || d.contains("setup-token")
        || d.contains("not logged in")
        || d.contains("please log in")
        || d.contains("login")
}

/// Classify a provider's auth state from (cli-present?, probe ok?, probe detail).
/// Pure: the caller does the presence check + probe and passes the booleans in.
/// `probe`: `None` = not probed (treated as Down); `Some((ok, detail))` = probed.
pub fn classify(found: bool, probe: Option<(bool, &str)>) -> ProviderAuth {
    if !found {
        return ProviderAuth::CliMissing;
    }
    match probe {
        Some((true, _)) => ProviderAuth::Ready,
        Some((false, detail)) if is_auth_failure(detail) => ProviderAuth::NeedsLogin(detail.to_string()),
        Some((false, detail)) => ProviderAuth::Down(detail.to_string()),
        None => ProviderAuth::Down("not probed".to_string()),
    }
}

/// The login command to get a provider authenticated (the "guide" half).
pub fn login_command(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "run `claude setup-token`, then export CLAUDE_CODE_OAUTH_TOKEN=<token> (add it to your shell profile so it persists)",
        Provider::Codex => "run `codex login`",
        Provider::Gemini => "run `agy login`",
    }
}

/// A one-line, actionable guidance string for a provider's status.
pub fn guidance(p: Provider, status: &ProviderAuth) -> String {
    let bin = cli_binary(p);
    match status {
        ProviderAuth::Ready => format!("{bin}: ready"),
        ProviderAuth::CliMissing => {
            format!("{bin}: not installed — install the {bin} CLI and ensure it's on your PATH")
        }
        ProviderAuth::NeedsLogin(_) => format!("{bin}: {}", login_command(p)),
        ProviderAuth::Down(detail) => {
            format!("{bin}: {detail} — retry, or run `{bin} -p hi` directly to see the error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_binary_maps_each_provider() {
        assert_eq!(cli_binary(Provider::Claude), "claude");
        assert_eq!(cli_binary(Provider::Codex), "codex");
        assert_eq!(cli_binary(Provider::Gemini), "agy");
    }

    #[test]
    fn is_auth_failure_matches_auth_shaped_details() {
        assert!(is_auth_failure("API Error: 401 authentication_error"));
        assert!(is_auth_failure("invalid credentials"));
        assert!(is_auth_failure("Please log in to continue"));
        assert!(is_auth_failure("run claude setup-token"));
        // Non-auth failures are NOT auth-shaped:
        assert!(!is_auth_failure("rate limit exceeded"));
        assert!(!is_auth_failure("connection timed out"));
    }

    #[test]
    fn classify_missing_cli() {
        assert_eq!(classify(false, None), ProviderAuth::CliMissing);
        // Even with a probe result, a missing CLI is CliMissing.
        assert_eq!(classify(false, Some((true, "ok"))), ProviderAuth::CliMissing);
    }

    #[test]
    fn classify_ready_needs_login_and_down() {
        assert_eq!(classify(true, Some((true, "ok"))), ProviderAuth::Ready);
        assert_eq!(
            classify(true, Some((false, "401 unauthorized"))),
            ProviderAuth::NeedsLogin("401 unauthorized".to_string())
        );
        assert_eq!(
            classify(true, Some((false, "rate limit exceeded"))),
            ProviderAuth::Down("rate limit exceeded".to_string())
        );
        assert_eq!(classify(true, None), ProviderAuth::Down("not probed".to_string()));
    }

    #[test]
    fn guidance_gives_login_command_for_needs_login() {
        let g = guidance(Provider::Claude, &ProviderAuth::NeedsLogin("401".into()));
        assert!(g.contains("setup-token"), "got: {g}");
        let g = guidance(Provider::Codex, &ProviderAuth::NeedsLogin("401".into()));
        assert!(g.contains("codex login"), "got: {g}");
        let g = guidance(Provider::Gemini, &ProviderAuth::NeedsLogin("401".into()));
        assert!(g.contains("agy login"), "got: {g}");
    }

    #[test]
    fn guidance_for_missing_says_install() {
        let g = guidance(Provider::Codex, &ProviderAuth::CliMissing);
        assert!(g.contains("not installed") && g.contains("PATH"), "got: {g}");
    }

    #[test]
    fn guidance_for_down_echoes_detail_not_login() {
        let g = guidance(Provider::Gemini, &ProviderAuth::Down("rate limited".into()));
        assert!(g.contains("rate limited"), "got: {g}");
        assert!(!g.contains("agy login"), "Down must not suggest re-login: {g}");
    }
}
```

- [ ] **Step 3: Run the tests — verify they pass.**

Run: `cargo test -p consilium auth::`
Expected: PASS (7 tests).

- [ ] **Step 4: Gate + commit** (NO Co-Authored-By trailer — attribution is disabled in this repo):

```bash
cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --check
git add core/src/auth.rs core/src/lib.rs
git commit -m "feat(auth): pure provider auth-status classifiers + guidance"
```

---

### Task 2: Probe I/O shell

Adds the I/O functions that feed `classify`, reusing `doctor`. Thin and not unit-tested (spawns CLIs), exactly like `doctor::probe_model`.

**Files:**
- Modify: `core/src/auth.rs` (append below the pure core, above `#[cfg(test)]`).

- [ ] **Step 1: Add the imports + I/O functions.** At the top of `auth.rs`, extend the imports:

```rust
use crate::catalog::catalog;
use crate::doctor;
use crate::quota::QuotaStore;
```

Then add (after `guidance`, before the `#[cfg(test)]` module):

```rust
/// The model probed to test a provider's auth — its first catalog entry (the
/// curated primary). Every v1 provider has at least one catalog entry.
pub fn primary_model(p: Provider) -> Option<String> {
    catalog().into_iter().find(|e| e.provider == p).map(|e| e.model)
}

/// Probe one provider's auth state: CLI presence (`doctor::check`) then, if
/// present, a live liveness probe (`doctor::probe_model`, ~1 token) on its
/// primary catalog model. I/O — not unit-tested (spawns a real CLI).
pub async fn probe_auth(p: Provider, quota: &QuotaStore) -> ProviderAuth {
    let bin = cli_binary(p);
    if !doctor::check(bin).found {
        return ProviderAuth::CliMissing;
    }
    let Some(model) = primary_model(p) else {
        // No catalog entry to probe — treat as Down with a clear reason.
        return ProviderAuth::Down(format!("no catalog model for {}", p.as_str()));
    };
    let adapter = doctor::adapter_for(p);
    let probe = doctor::probe_model(adapter, &model, quota).await;
    classify(true, Some((probe.ok, &probe.detail)))
}

/// Probe all v1 providers concurrently, so a cold-starting Claude (~30s) does not
/// serialize the wait. Returns one (provider, status) per v1 provider, in a
/// stable order (claude, codex, gemini).
pub async fn auth_report(quota: &QuotaStore) -> Vec<(Provider, ProviderAuth)> {
    let providers = [Provider::Claude, Provider::Codex, Provider::Gemini];
    let futs = providers
        .into_iter()
        .map(|p| async move { (p, probe_auth(p, quota).await) });
    futures::future::join_all(futs).await
}
```

- [ ] **Step 2: Add a unit test for the one pure addition** (`primary_model`) to the test module:

```rust
#[test]
fn primary_model_is_the_first_catalog_entry_per_provider() {
    assert_eq!(primary_model(Provider::Claude).as_deref(), Some("claude-opus-4-8"));
    assert_eq!(primary_model(Provider::Codex).as_deref(), Some("gpt-5.4"));
    assert!(primary_model(Provider::Gemini).is_some());
}
```

- [ ] **Step 3: Run tests + gate.**

Run: `cargo test -p consilium auth::` (8 tests now), then `cargo clippy --all-targets --all-features -- -D warnings` && `cargo fmt --check`.
Expected: PASS, zero warnings. (The async I/O fns compile + are covered indirectly via the CLI command's dogfood in Task 3.)

- [ ] **Step 4: Commit:**

```bash
git add core/src/auth.rs
git commit -m "feat(auth): probe_auth + concurrent auth_report (reuses doctor probe)"
```

---

### Task 3: `consilium auth` command

**Files:**
- Modify: `core/src/main.rs` (the `Command` enum + the `match` in `main`).
- Test: real-CLI dogfood (manual, recorded in the commit message / PR).

- [ ] **Step 1: Add the subcommand variant.** In the `Command` enum ([main.rs:15](../../core/src/main.rs)), add after the `Doctor { … }` variant:

```rust
    /// Report each provider's auth state and the exact next step to authenticate
    /// it (probes liveness — spends ~1 token per provider).
    Auth {
        /// Probe just one provider (claude|codex|gemini) instead of all.
        #[arg(long)]
        provider: Option<String>,
    },
```

- [ ] **Step 2: Add the handler.** In `main`'s `match command` block, add an arm (mirror how `Command::Doctor { models: true }` opens the quota store — read main.rs around the Doctor arm, ~lines 140-170, for the exact `quota_db_path()` + `QuotaStore::open` calls):

```rust
        Command::Auth { provider } => {
            let store = consilium::quota::QuotaStore::open(quota_db_path()?)?;
            let report = match provider {
                Some(name) => {
                    let p: consilium::event::Provider = name
                        .parse()
                        .map_err(|e| anyhow::anyhow!("unknown provider '{name}': {e}"))?;
                    vec![(p, consilium::auth::probe_auth(p, &store).await)]
                }
                None => consilium::auth::auth_report(&store).await,
            };
            let ready = report
                .iter()
                .filter(|(_, s)| matches!(s, consilium::auth::ProviderAuth::Ready))
                .count();
            println!("── provider auth ──");
            for (p, status) in &report {
                let mark = if matches!(status, consilium::auth::ProviderAuth::Ready) {
                    "✓"
                } else {
                    "✗"
                };
                println!("  {mark} {}", consilium::auth::guidance(*p, status));
            }
            println!("{ready}/{} providers ready", report.len());
        }
```

> Verify the exact `QuotaStore::open(...)` signature against the Doctor arm — if it differs (e.g. takes a `&Path` or returns differently), match that call exactly. The `Provider: FromStr` impl already exists ([event.rs:22](../../core/src/event.rs)) and errors on unknown names.

- [ ] **Step 3: Build + verify the command compiles and runs.**

Run: `cargo build -p consilium && cargo run -q -p consilium -- auth --help`
Expected: builds; help shows the `--provider` flag.

- [ ] **Step 4: Gate.**

Run: `cargo test -p consilium` && `cargo clippy --all-targets --all-features -- -D warnings` && `cargo fmt --check`.
Expected: all PASS, zero warnings.

- [ ] **Step 5: Real dogfood** (operator-run, spends a few tokens): `cargo run -q -p consilium -- auth`. Confirm it prints a status line per provider and, for any not-ready one, the correct guidance (e.g. Claude not authed → the `claude setup-token` hint). Note the observed output in the commit body.

- [ ] **Step 6: Commit:**

```bash
git add core/src/main.rs
git commit -m "feat(cli): consilium auth — per-provider auth status + guidance"
```

---

### Task 4: Docs, gate & merge

**Files:**
- Modify: `README.md` (roadmap row), `/Users/temur/.claude/projects/-Users-temur-Desktop-Claude/memory/project_consilium.md`.

- [ ] **Step 1: Full gate.** `cargo test -p consilium` && `cargo clippy --all-targets --all-features -- -D warnings` && `cargo fmt --check`. All green.

- [ ] **Step 2: README roadmap row.** Add after the "Onboarding foundation" row:

```
| **Auth orchestrator** | `consilium auth` — probes each provider's liveness and prints the exact "detect + guide" next step (`claude setup-token` / `codex login` / `agy login`); concurrent probes | ✅ Done — the `init` wizard (slice 4) consumes it |
```

- [ ] **Step 3: Memory.** In `project_consilium.md`, mark slice 3 (`consilium auth`) done with the commit shas; note slice 4 (the interactive `init` wizard, Plan B) as the next consumer.

- [ ] **Step 4: Commit + merge** per the repo's branch → gate → `merge --no-ff` → push workflow.

```bash
git add README.md
git commit -m "docs(auth): document the consilium auth orchestrator"
```

---

## Self-review

**1. Spec coverage.** Design slice 3 requires: `ProviderAuth` status (Task 1), `probe_auth` reusing doctor (Task 2), `guidance` per provider incl. CliMissing install hint (Task 1), `consilium auth` command with concurrent probing (Tasks 2-3), idempotent/re-runnable (it's a stateless probe — re-running re-probes). All covered. The `ProviderAuth::Down` arm (non-auth failure) from the spec's error-handling section is implemented + tested. ✓

**2. Placeholder scan.** No TBD/TODO. Every code step is complete. The one soft pointer — "match the Doctor arm's exact `QuotaStore::open` call" — is a concrete instruction to read a specific nearby handler, not a vague placeholder; the engineer copies a real existing line. ✓

**3. Type consistency.** `ProviderAuth` (4 variants) defined in Task 1, consumed in Tasks 2-3 with matching variant names. `classify(found: bool, probe: Option<(bool, &str)>)` is consistent between definition (Task 1) and caller (`probe_auth` in Task 2 passes `Some((probe.ok, &probe.detail))`). `guidance(Provider, &ProviderAuth)`, `cli_binary(Provider)`, `login_command(Provider)`, `primary_model(Provider) -> Option<String>` all consistent across tasks and the CLI handler. `doctor::check`/`probe_model`/`adapter_for` and `ModelProbe{ok, detail}` match the real [doctor.rs](../../core/src/doctor.rs). ✓

**4. Dependency check.** `futures` is already in `core/Cargo.toml` (used by `council.rs`'s `join_all`). No new dep in Plan A (`dialoguer` arrives in Plan B). ✓
