#!/usr/bin/env bash
# Records REAL CLI outputs for parser verification.
# Spends a few real requests — run manually, never in CI.
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p core/tests/fixtures/{claude,codex,gemini}/recorded

record() {
  local name="$1" dest="$2"; shift 2
  local tmp; tmp=$(mktemp)
  # < /dev/null: codex exec hangs reading stdin when stdout is redirected;
  # harmless for the other CLIs (sessions.rs uses Stdio::null for the same reason).
  if "$@" < /dev/null > "$tmp" 2> >(cat >&2); then
    mv "$tmp" "$dest"
    echo "$name: recorded"
  else
    echo "$name: FAILED" >&2
    rm -f "$tmp"
  fi
}

record claude core/tests/fixtures/claude/recorded/basic.jsonl \
  claude -p 'Reply with exactly: ok' --output-format stream-json --verbose
record codex core/tests/fixtures/codex/recorded/basic.jsonl \
  codex exec --json 'Reply with exactly: ok'
record gemini core/tests/fixtures/gemini/recorded/basic.json \
  gemini -p 'Reply with exactly: ok' --output-format json

echo "Now diff recorded vs synthetic fixtures; update parsers if formats drifted."
