# Consilium — Pick-Your-Council Onboarding & Self-Updating Model Pool (design)

*Status: DRAFT for review (2026-06-23). Brainstormed pre-compaction; implement post-compaction via writing-plans. Each slice gets its own plan → implementation cycle.*

## Problem & goal

After install, get a user from zero to a working council with **minimal effort**, requiring **at least one authenticated provider** — without any authenticated agent the product is "brainless." Smart defaults (Consilium's curated recommendations) assign the best model per role; the user can override. The available-model pool and the recommendations must **stay current as models ship weekly**, without a Consilium release per model launch.

Resolves the prior open question: **cross-family conductor is no longer a fixed principle — it's a role choice.** Default = Claude conducts (the owner's preference, now just the default), but the user can assign any authed model to any role.

## The post-install journey (every step)

1. **Install** → `consilium` binary present, no config yet.
2. **First run / `consilium init`** → no config detected ⇒ launch the onboarding wizard.
3. **Auth gate (≥1 required).** Probe which provider CLIs are installed/authed. The user MUST authenticate at least one before continuing. `consilium auth` orchestrates per-provider login:
   - `claude` → run `claude /login` or guide `claude setup-token` → export `CLAUDE_CODE_OAUTH_TOKEN`.
   - `codex` → `codex login`.
   - `agy` (Antigravity → Gemini + Claude + GPT-OSS) → `agy` login.
   - API-key providers (future: GLM/DeepSeek/Kimi) → prompt for key → store in gitignored `~/.consilium/secrets.env`.
   - Consilium cannot perform interactive OAuth itself; it **spawns/guides** each CLI's flow, then **verifies via probe**.
4. **Capability discovery.** For each authed provider, query its live model list (`agy models`, etc.) → the **available-model set** = (live CLI models) ∩ (curated catalog).
5. **Role assignment** — two paths:
   - **Default (recommended):** user picks "Default" → Consilium auto-assigns best-model-per-role from the **recommendations**, constrained to *authed + available* models. If only one provider is authed, every role uses it (degraded but functional — never brainless).
   - **Custom:** user assigns a model per role from the authed+available catalog (this is where cross-family conductor becomes a choice).
6. **Write `consilium.config.json` + verify.** `doctor --models` probes the resolved ladders; if a chosen model is unreachable, offer a swap.
7. **Ready.** `conduct` / `auto` / `council` work.

**Ordering decision (settled):** roles/catalog first → auth only the chosen providers → probe/refine. Showing the catalog needs no auth; we only ask the user to authenticate providers their picks actually use (minimal effort). Auth-first would force authenticating everything before the user knows what they want.

## Components (each a focused unit)

- **Provider catalog** (`catalog.rs`): curated, in-binary list of providers × models, each with `auth_method` (cli-login | setup-token | api-key), per-role **recommendation scores** (conductor/worker/reviewer/supervisor), and tier/cost hints. The source of "our recommendations." Remotely refreshable (below).
- **Auth orchestrator** (`consilium auth`): per-provider login flows + `secrets.env` management + probe verification. Idempotent; re-runnable to add a provider.
- **Onboarding wizard** (`consilium init`, interactive): the journey above. CLI-first (the install entry point); web-UI onboarding is a later track.
- **Recommendation resolver**: given authed+available models + the catalog scores, produce the default role→model assignment (+ failover ladders, e.g. cross-family reviewer). Constrained, deterministic, unit-testable.
- **Model-pool updater**: keeps available models + recommendations current as models ship weekly (below).

## Self-updating model pool (the "self-improving" system)

Two distinct questions, two mechanisms:
- **"What models exist right now?"** → **live CLI discovery** (`agy models`, etc.) at `init`/`doctor`. Always current — the CLI is the source of truth for its own models. No release needed.
- **"Which model is best per role?"** (the recommendations) → a **versioned recommendations catalog** shipped in-binary AND **refreshable from a remote JSON** (e.g. a versioned file in the Consilium GitHub repo), cached in `~/.consilium/`, refreshed on a cadence (init/doctor or weekly), with **in-binary fallback** when offline.

Result: new models appear automatically via discovery; the "best per role" guidance refreshes from the remote catalog — neither needs a Consilium binary release per weekly model launch.

**Phase 2 (self-improving, true):** feed `consilium eval` results back into the recommendations — which model *actually* performs best per role/task-type — calibrating the catalog from real benchmarks. Ties directly into the deferred **routing-bias table** (TRINITY borrow #7). Out of scope for v1; the catalog schema should leave room for an `observed_score` field.

## Decisions (revisit at review)

- **v1 providers:** claude / codex / agy (already integrated). Chinese models (GLM / DeepSeek / Kimi) = adapter follow-up — the catalog + auth frame makes adding them mechanical (CLI or OpenAI-compatible API, like the agy adapter).
- **Onboarding surface:** CLI `consilium init` wizard is v1 (the entry point). Web-UI onboarding is a later track.
- **Recommendations catalog hosting:** versioned JSON in the Consilium GitHub repo (simple, free, diff-able), fetched raw + cached, in-binary fallback.
- **Self-improving v1 = discovery + catalog refresh**; eval-calibration is phase 2.

## Open forks for the sync

1. **Remote catalog host:** GitHub raw JSON (lean) vs a dedicated endpoint. (Recommend GitHub raw.)
2. **Self-improving depth for v1:** discovery + refresh only, or also the eval-calibration loop. (Recommend phase-1 only for v1.)
3. **Auth UX scope:** CLI wizard only, or also web-UI onboarding in v1. (Recommend CLI first.)

## Implementation slices (post-compaction, each its own plan)

1. **Provider catalog** — static in-binary catalog with per-role recommendation scores + auth-method metadata + tests.
2. **Recommendation resolver** — authed+available → default role→model + ladders (cross-family reviewer); deterministic, unit-tested.
3. **Auth orchestrator** (`consilium auth`) — per-provider login orchestration + `secrets.env` + probe verification.
4. **Onboarding wizard** (`consilium init` interactive) — auth gate (≥1) → discover → assign (default|custom) → write config → verify.
5. **Model-pool updater** — live CLI discovery + remotely-refreshable recommendations catalog + cache + offline fallback.
6. **(Phase 2)** eval-calibrated recommendations → routing-bias table.

## Non-goals (v1)

- No web-UI onboarding (CLI wizard only).
- No automatic OAuth (we orchestrate the CLIs' own flows).
- No eval-calibration loop (phase 2).
- No Chinese-model adapters yet (the frame enables them; building them is a follow-up).
