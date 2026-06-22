# Consilium — Auth Orchestrator + Onboarding Wizard (design)

*Status: APPROVED (2026-06-23). Refines slices 3+4 of the product-UX onboarding milestone ([2026-06-23-product-ux-onboarding-design.md](2026-06-23-product-ux-onboarding-design.md)). Builds on the merged catalog + `recommend_roles` (44d891b). Next: writing-plans.*

## Problem & goal

Slices 1+2 shipped the pure foundation — a curated provider **catalog** and a **`recommend_roles`** resolver. But nothing yet (a) helps a user *authenticate* providers, or (b) turns the recommendation into a written `consilium.config.json`. `consilium init` still writes a hardcoded `Config::default()` regardless of what's authed. This design adds the **auth orchestrator** (slice 3) and the **interactive onboarding wizard** (slice 4) so a fresh user goes from install → a working, authed council with minimal effort, requiring **≥1 authenticated provider**.

## Settled decisions

- **Auth flow = detect + guide** (user choice). Consilium never hijacks the terminal or drives interactive OAuth itself. It detects auth state and, for anything not ready, prints the **exact command for the user to run**, then re-checks. Robust, testable, works headless/over SSH.
- **Ready check = live probe** (user choice). "Authenticated" is confirmed by a real liveness probe (reuse `doctor::probe_model` — a tiny "reply ok", ~1 token/provider), not just a credential-file heuristic. Definitive; the user accepted the small one-time onboarding token cost.
- **Prompts = `dialoguer` crate** (recommended, approved). Idiomatic Rust `Select`/`Confirm` menus. One new dependency, justified for a user-facing wizard.
- **Degrade, don't block.** A provider the user skips or can't auth is dropped; the lineup re-resolves over whatever IS ready (`recommend_roles(available)`). Never brainless: the wizard stops only if **zero** providers are ready.
- **`init` becomes the wizard.** `consilium init` (no flags) runs the interactive wizard; `consilium init --yes` writes the recommended Default lineup non-interactively (CI/scripts) — supersedes today's plain writer.

## Components

### Auth module — `core/src/auth.rs` (slice 3)

One provider → one status, reusing `doctor` primitives (no new probing logic):

```rust
pub enum ProviderAuth {
    Ready,                 // probe succeeded — the provider answers
    NeedsLogin(String),    // CLI present but probe failed auth-shaped (reason carried)
    CliMissing,            // binary not on PATH
    Down(String),          // present, probe failed non-auth (transient/other)
}
```

- **`probe_auth(provider, quota) -> ProviderAuth`** (I/O shell, not unit-tested — like `probe_model`): `doctor::check(binary)` → if absent, `CliMissing`; else `doctor::probe_model(adapter, primary_model, quota)` on the provider's **primary catalog model** → `Completed` = `Ready`; failure classified `ModelUnavailable`/auth-shaped (via the existing `remediation_hint`-style matching) = `NeedsLogin`; other = `Down`.
- **`guidance(provider, status) -> String`** (pure, unit-tested): the exact next step.
  - Claude → "run `claude setup-token`, then `export CLAUDE_CODE_OAUTH_TOKEN=…` (add it to your shell profile)".
  - Codex → "run `codex login`".
  - Gemini → "run `agy login`".
  - `CliMissing` → prepend the install hint for that CLI.
  Sourced from the catalog entry's `AuthMethod`.
- **`consilium auth [--provider <name>]`** command (in `main.rs`): probes all v1 providers (or the named one) **concurrently** (`futures::join_all`, so a ~30s Claude cold-start doesn't serialize), prints a status line per provider + `guidance` for anything not `Ready`. Idempotent and re-runnable — the user runs it, follows the hints, re-runs until green.

### Wizard — interactive `consilium init` (slice 4)

Roles-first flow (no auth needed to *show* the catalog):

1. Print a short intro and **preview the recommended Default lineup** (`recommend_roles(catalog())`).
2. `Select`: **Use Default (recommended)** or **Customize per role**.
3. Determine **candidate providers**: Default → the providers in the previewed lineup; Custom → the providers behind the user's per-role picks.
4. **Auth gate** — for each candidate provider, `probe_auth`; if not `Ready`, print `guidance` and `Select` `[Re-check]/[Skip this provider]/[Quit]`, looping until `Ready` or skipped.
5. Require **≥1 `Ready`** provider (else exit with the "authenticate at least one provider" message + `consilium auth` hint). `available` = catalog entries whose provider is `Ready`.
6. **Default** → `recommend_roles(available)`. **Custom** → `Select` a model per role from `available` (each role's menu defaults to the recommended pick).
7. **Write** `consilium.config.json` from the resolved `RolesConfig` (the rest of `Config` keeps its defaults). `Confirm` before overwriting an existing file.
8. Run a final preflight (the chosen ladders) and print a summary; point to `consilium doctor --models`.

`consilium init --yes` skips the wizard: writes `recommend_roles(catalog())` (the full curated lineup) without probing — today's non-interactive behavior, now sourced from the resolver.

## Data flow

`catalog()` → (preview) `recommend_roles(catalog())` shown to user → user picks Default|Custom → `probe_auth` per candidate provider → `available` (Ready ∩ catalog) → `recommend_roles(available)` | custom picks → `RolesConfig` → `Config` → `consilium.config.json` → preflight verify.

## Error handling

- **Zero ready providers** → hard stop, exit non-zero, print the `consilium auth` guidance for each candidate. The product is brainless without an agent (spec invariant).
- **Probe is slow/cold** (Claude ~30s+) → the 120s probe timeout already covers it; concurrent probing keeps the wall-clock down.
- **`Down` (non-auth failure)** during the gate → treated like `NeedsLogin` for flow purposes (offer Re-check/Skip), but the printed reason is the transient detail, not a login hint.
- **Overwrite guard** → `Confirm` before clobbering an existing `consilium.config.json`; `--yes` overwrites without asking (matches today's `--force` intent).
- **`dialoguer` on a non-TTY** (piped stdin) → detect and fall back to the `--yes` path (or error clearly), so the wizard never hangs in CI.

## Testing

Mirror `doctor.rs`'s split — pure helpers tested, I/O shell not:
- **Unit-tested (pure):** `guidance` (each provider + `CliMissing`), auth-status classification from a probe outcome, "providers needed by a `RolesConfig`" helper, and the `--yes` config assembly. `recommend_roles` is already covered.
- **Not unit-tested (thin I/O):** `probe_auth` (spawns a CLI) and the `dialoguer` interactive loop — same rationale as `probe_model`. A real-CLI dogfood (`consilium auth`, then `consilium init` against a scratch dir) is the integration proof, run by the operator.

## Slice split for implementation plans

- **Plan A — auth module (slice 3):** `auth.rs` (`ProviderAuth`, `probe_auth`, `guidance`) + `consilium auth` command + unit tests. Self-contained; ships a usable `consilium auth`.
- **Plan B — wizard (slice 4):** `dialoguer` dep, the interactive `init` flow consuming `auth` + `recommend_roles`, the `--yes` path, overwrite/TTY guards. Depends on Plan A.

Each plan → its own subagent-driven implementation + review + merge.

## Non-goals (this design)

- No web-UI onboarding (CLI only).
- No automatic/driven OAuth (we guide; the user runs the login).
- No API-key providers yet (the `AuthMethod::ApiKey` arm + `secrets.env` writing is a follow-up when Chinese models land).
- No model-pool updater / remote catalog refresh (slice 5, separate).
