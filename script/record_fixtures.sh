#!/usr/bin/env bash
# Records REAL CLI outputs for parser verification.
# Spends a few real requests — run manually, never in CI.
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p core/tests/fixtures/{claude,codex,gemini}/recorded

claude -p 'Reply with exactly: ok' --output-format stream-json --verbose \
  > core/tests/fixtures/claude/recorded/basic.jsonl 2>/dev/null \
  && echo "claude: recorded" || echo "claude: FAILED"

codex exec --json 'Reply with exactly: ok' \
  > core/tests/fixtures/codex/recorded/basic.jsonl 2>/dev/null \
  && echo "codex: recorded" || echo "codex: FAILED (not installed?)"

gemini -p 'Reply with exactly: ok' --output-format json \
  > core/tests/fixtures/gemini/recorded/basic.json 2>/dev/null \
  && echo "gemini: recorded" || echo "gemini: FAILED"

echo "Now diff recorded vs synthetic fixtures; update parsers if formats drifted."
