# Provider expansion research: Grok, GLM, Kimi, DeepSeek

**Date:** 2026-07-07 · four parallel web-research agents, primary sources only.
**Question:** which providers can join the council, and by which of two paths:
- **Path A — own official CLI**: new adapter parsing their stream format (like claude/codex/agy).
- **Path B — Anthropic-compatible endpoint**: reuse the existing `claude` CLI + adapter with
  `ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN`/`ANTHROPIC_MODEL` env overrides at spawn.

## Summary table

| Provider | Official CLI | Headless JSON | Subscription auth | Cheapest seat | Path | Difficulty |
|---|---|---|---|---|---|---|
| **Kimi (Moonshot)** | `kimi` (MoonshotAI/kimi-cli, v1.48.0, 9.1k★, Apache-2.0) | `--print --output-format stream-json` (JSONL), stdin driving, exit codes 0/1/75 | browser OAuth against Kimi membership | ~$19/mo (Moderato) | A (B also official) | easy |
| **Grok (xAI)** | `grok` (Grok Build, beta since May 2026) | `-p --output-format streaming-json` (NDJSON), `--always-approve`, sessions `-s/-r/-c` | browser OAuth via X Premium+ / SuperGrok | $30/mo SuperGrok (shared weekly pool) | A | easy (beta schema churn) |
| **GLM (Z.ai)** | none (ZCode is a desktop app, no headless) | n/a | API key issued by Coding Plan | Lite $18/mo (~$12.60 promo), ~80 prompts/5h | B (their primary marketed path) | easy |
| **DeepSeek** | none (community deepcode-cli, no headless) | n/a | **no subscription — pay-per-token only** | v4-flash $0.14/$0.28 per 1M | B, metered | easy, economics caveat |

## Key facts

### Grok Build (xAI)
- Official docs: docs.x.ai/build/overview, headless: docs.x.ai/build/cli/headless-scripting.
- `grok -p "<prompt>" --output-format streaming-json --always-approve --no-auto-update`.
- OAuth token cached from browser login tied to X Premium+/SuperGrok; `XAI_API_KEY` exists
  for CI but is metered — the subscription login is the Consilium-fit path.
- Since June 2026 paid plans share one weekly usage pool across all Grok products.
- Model grok-build-0.1 (~70.8% SWE-Bench Verified). Anthropic-SDK-compatible API exists but
  only with metered keys — fallback, not the plan.

### Kimi Code CLI (Moonshot)
- The closest analog to our existing adapters: JSONL out, JSONL in, retryable-vs-permanent
  exit codes. Maintained actively (release June 22, 2026).
- `/login` → "Kimi Code" = OAuth against the flat-rate membership. Free "Adagio" chat plan
  does NOT include Kimi Code; paid from ~$19/mo. Exact credit quotas unpublished.
- Bonus: same subscription officially backs Claude Code via api.kimi.com/coding/.

### GLM Coding Plan (Z.ai)
- No first-party CLI. Their official, actively marketed integration IS Path B:
  docs.z.ai/scenario-example/develop-tools/claude → `ANTHROPIC_BASE_URL=https://api.z.ai/api/anthropic`.
- Server-side model mapping: opus/sonnet→GLM-4.7, haiku→GLM-4.5-Air (overridable).
- Lite $18/mo (30% promo to Sept 2026): ~80 prompts/5h, ~400/week. GLM-5.2 burns quota at
  3x peak / 2x off-peak — default GLM-4.7 is 1x.

### DeepSeek
- No official CLI; community deepcode-cli has no headless mode — nothing to parse.
- Official Anthropic-compatible endpoint: api.deepseek.com/anthropic with a dedicated
  Claude Code guide (`ANTHROPIC_MODEL=deepseek-v4-pro`; opus→v4-pro, sonnet/haiku→v4-flash).
- No flat-rate plan: pay-per-token only (very cheap; one-time signup grant ~5M tokens).
  Breaks the "subscriptions, not API keys" principle — if added, it must be an explicitly
  opt-in **metered seat** with a cost cap, visually distinct in any UI.

## Recommended integration order

1. **Kimi** — official CLI, subscription OAuth, JSONL that maps ~1:1 onto our parsing. Cheapest new-adapter work.
2. **Env-override mechanism** (one core feature: `Provider` variants that spawn the `claude`
   binary with injected env) — unlocks **GLM** immediately and DeepSeek later; also a
   universal fallback for any future Anthropic-compatible vendor.
3. **Grok** — official CLI + $30/mo flat rate fits the thesis; schema is beta, so pin
   fixtures from recorded real output (existing discipline) and expect churn.
4. **DeepSeek** — last, only as an explicitly metered opt-in seat with a budget cap.

## Consequences for the core

- New `Provider` variants must not assume 1 provider = 1 CLI binary: GLM/DeepSeek spawn
  `claude` with env overrides; quota accounting keys must stay per *subscription pool*,
  not per binary.
- `sessions::spawn` needs per-rung env injection (small, isolated change).
- Catalog entries need an `auth` shape for "key from vendor console" (GLM) alongside
  OAuth logins.
- Multi-worker-per-provider (e.g. two Anthropic models as separate workers) already works
  in `RolesConfig.workers`; init/recommend flow and UI need to allow N seats per family,
  and any fatigue/quota display must be per family (shared window), not per seat.
