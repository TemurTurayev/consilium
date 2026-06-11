# Consilium M1: Engine Foundation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working `consilium` binary with `doctor`, `run`, and `quota` commands: normalized event streaming from claude/codex/gemini CLIs via adapters, SQLite usage tracking.

**Architecture:** Single Rust workspace. Adapters are pure parsers (`parse_line`/`parse_final`: text → `Vec<AgentEvent>`) tested on fixtures without spending quota; process spawning/streaming lives in `sessions.rs`; usage counters in `quota.rs` (rusqlite). M2 (orchestration) and M3 (server+UI) build on these types.

**Tech Stack:** Rust (tokio, clap, serde, rusqlite bundled, tracing), no network deps in M1.

**Repo:** `/Users/temur/Desktop/Claude/consilium` (already has docs/, .gitignore, git history).

**Important reality check:** synthetic fixtures below encode the *expected* CLI output formats. Task 4 records REAL outputs via `script/record_fixtures.sh`. If recorded output differs from synthetic fixtures, update the synthetic fixtures AND parsers to match reality — the `AgentEvent` mapping is the contract, the raw format is not.

---

### Task 0: Install Rust toolchain

**Files:** none (system setup)

- [ ] **Step 1: Install rustup non-interactively**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

- [ ] **Step 2: Verify**

Run: `cargo --version && rustc --version`
Expected: `cargo 1.x.x` and `rustc 1.x.x` (stable, ≥1.85)

### Task 1: Cargo workspace scaffold + CLI skeleton

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `core/Cargo.toml`
- Create: `core/src/main.rs`
- Create: `core/src/lib.rs`

- [ ] **Step 1: Write workspace root `Cargo.toml`**

```toml
[workspace]
members = ["core"]
resolver = "2"
```

- [ ] **Step 2: Write `core/Cargo.toml`**

```toml
[package]
name = "consilium"
version = "0.1.0"
edition = "2021"
description = "Multi-agent orchestrator for subscription CLI agents"
license = "MIT"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
rusqlite = { version = "0.32", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

- [ ] **Step 3: Write `core/src/lib.rs`** (modules added in later tasks stay commented until created)

```rust
pub mod event;
// pub mod config;     // Task 3
// pub mod adapters;   // Task 4
// pub mod sessions;   // Task 7
// pub mod quota;      // Task 8
// pub mod doctor;     // Task 9
```

Note: `pub mod event;` requires `core/src/event.rs` to exist — create it as an empty file in this task: `touch core/src/event.rs`.

- [ ] **Step 4: Write `core/src/main.rs`**

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "consilium", version, about = "Multi-agent orchestrator for subscription CLI agents")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that agent CLIs are installed and authenticated
    Doctor,
    /// Run a single prompt through one agent (smoke test)
    Run {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: Option<String>,
        prompt: String,
    },
    /// Show usage counters per provider
    Quota,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::from_default_env(),
    ).init();
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor => println!("doctor: not implemented yet"),
        Command::Run { provider, model, prompt } => {
            println!("run: not implemented yet ({provider}, {model:?}, {prompt})");
        }
        Command::Quota => println!("quota: not implemented yet"),
    }
    Ok(())
}
```

- [ ] **Step 5: Build and smoke-run**

Run: `cargo build && cargo run -- doctor`
Expected: compiles; prints `doctor: not implemented yet`

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: cargo workspace + CLI skeleton (doctor/run/quota)"
```

### Task 2: AgentEvent + Provider types (TDD)

**Files:**
- Modify: `core/src/event.rs`

- [ ] **Step 1: Write failing tests** (bottom of `core/src/event.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_parses_from_str() {
        assert_eq!("claude".parse::<Provider>().unwrap(), Provider::Claude);
        assert_eq!("codex".parse::<Provider>().unwrap(), Provider::Codex);
        assert_eq!("gemini".parse::<Provider>().unwrap(), Provider::Gemini);
        assert!("warp".parse::<Provider>().is_err());
    }

    #[test]
    fn agent_event_serializes_with_snake_case_tag() {
        let ev = AgentEvent::Usage { input_tokens: 10, output_tokens: 2 };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"usage","input_tokens":10,"output_tokens":2}"#);
    }

    #[test]
    fn agent_event_round_trips() {
        let ev = AgentEvent::SessionStarted {
            session_id: "s1".into(),
            provider: Provider::Claude,
            model: Some("fable-5".into()),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test -p consilium event`
Expected: COMPILE ERROR (types not defined yet)

- [ ] **Step 3: Implement types** (top of `core/src/event.rs`)

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
    Gemini,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Gemini => "gemini",
        }
    }
}

impl std::str::FromStr for Provider {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude" => Ok(Provider::Claude),
            "codex" => Ok(Provider::Codex),
            "gemini" => Ok(Provider::Gemini),
            other => Err(format!("unknown provider: {other}")),
        }
    }
}

/// Normalized event stream — the contract every adapter maps its CLI output into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionStarted {
        session_id: String,
        provider: Provider,
        model: Option<String>,
    },
    Thinking { text: String },
    Message { text: String },
    ToolCall { name: String, detail: String },
    FileChanged { path: String },
    Usage { input_tokens: u64, output_tokens: u64 },
    Completed { result: Option<String> },
    Failed { error: String },
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p consilium event`
Expected: 3 passed

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: AgentEvent and Provider core types"
```

### Task 3: Config loading (TDD)

**Files:**
- Create: `core/src/config.rs`
- Modify: `core/src/lib.rs` (uncomment `pub mod config;`)

- [ ] **Step 1: Write failing tests** (bottom of `core/src/config.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_when_file_missing() {
        let cfg = Config::load(Some(std::path::Path::new("/nonexistent/consilium.config.json"))).unwrap();
        assert_eq!(cfg.roles.conductor.provider, crate::event::Provider::Claude);
        assert!(!cfg.roles.workers.is_empty());
    }

    #[test]
    fn parses_spec_example() {
        let json = r#"{
          "roles": {
            "conductor":  { "provider": "claude", "model": "fable-5", "effort": "high", "mode": "attached" },
            "chairman":   { "provider": "claude", "model": "fable-5", "effort": "high" },
            "workers": [
              { "provider": "codex",  "model": "gpt-5.4" },
              { "provider": "gemini", "model": "gemini-3-pro" },
              { "provider": "claude", "model": "sonnet" }
            ],
            "reviewer":   { "provider": "codex",  "model": "gpt-5.4" },
            "supervisor": { "provider": "gemini", "model": "gemini-3-pro", "interventionThreshold": "medium" }
          },
          "quota": {
            "claude":  { "programmaticCreditUsd": 100 },
            "gemini":  { "dailyRequests": 1000 },
            "codex":   {}
          }
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.roles.workers.len(), 3);
        assert_eq!(cfg.roles.supervisor.intervention_threshold.as_deref(), Some("medium"));
        assert_eq!(cfg.quota.claude.programmatic_credit_usd, Some(100.0));
        assert_eq!(cfg.quota.gemini.daily_requests, Some(1000));
    }

    #[test]
    fn rejects_unknown_provider() {
        let json = r#"{"roles":{"conductor":{"provider":"warp","model":"x"},
            "chairman":{"provider":"claude","model":"x"},"workers":[],
            "reviewer":{"provider":"codex","model":"x"},
            "supervisor":{"provider":"gemini","model":"x"}}}"#;
        assert!(serde_json::from_str::<Config>(json).is_err());
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test -p consilium config`
Expected: COMPILE ERROR

- [ ] **Step 3: Implement** (top of `core/src/config.rs`)

```rust
use crate::event::Provider;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoleConfig {
    pub provider: Provider,
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub intervention_threshold: Option<String>,
}

impl RoleConfig {
    fn new(provider: Provider, model: &str) -> Self {
        Self { provider, model: model.into(), effort: None, mode: None, intervention_threshold: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RolesConfig {
    pub conductor: RoleConfig,
    pub chairman: RoleConfig,
    pub workers: Vec<RoleConfig>,
    pub reviewer: RoleConfig,
    pub supervisor: RoleConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeQuotaConfig {
    #[serde(default)]
    pub programmatic_credit_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiQuotaConfig {
    #[serde(default)]
    pub daily_requests: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CodexQuotaConfig {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct QuotaConfig {
    #[serde(default)]
    pub claude: ClaudeQuotaConfig,
    #[serde(default)]
    pub gemini: GeminiQuotaConfig,
    #[serde(default)]
    pub codex: CodexQuotaConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub roles: RolesConfig,
    #[serde(default)]
    pub quota: QuotaConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roles: RolesConfig {
                conductor: RoleConfig {
                    effort: Some("high".into()),
                    mode: Some("attached".into()),
                    ..RoleConfig::new(Provider::Claude, "fable-5")
                },
                chairman: RoleConfig {
                    effort: Some("high".into()),
                    ..RoleConfig::new(Provider::Claude, "fable-5")
                },
                workers: vec![
                    RoleConfig::new(Provider::Codex, "gpt-5.4"),
                    RoleConfig::new(Provider::Gemini, "gemini-3-pro"),
                ],
                reviewer: RoleConfig::new(Provider::Codex, "gpt-5.4"),
                supervisor: RoleConfig {
                    intervention_threshold: Some("medium".into()),
                    ..RoleConfig::new(Provider::Gemini, "gemini-3-pro")
                },
            },
            quota: QuotaConfig::default(),
        }
    }
}

impl Config {
    /// Load from path; missing file → defaults. Parse error → Err (never silently default).
    pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
        let Some(path) = path else { return Ok(Config::default()) };
        if !path.exists() {
            return Ok(Config::default());
        }
        let raw = std::fs::read_to_string(path)?;
        let cfg = serde_json::from_str(&raw)?;
        Ok(cfg)
    }
}
```

- [ ] **Step 4: Uncomment `pub mod config;` in `core/src/lib.rs`, run tests**

Run: `cargo test -p consilium config`
Expected: 3 passed

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: config loading with spec-compatible schema and defaults"
```

### Task 4: Adapter trait + Claude adapter (TDD, fixtures)

**Files:**
- Create: `core/src/adapters/mod.rs`
- Create: `core/src/adapters/claude.rs`
- Create: `core/tests/fixtures/claude/basic_session.jsonl`
- Create: `script/record_fixtures.sh`
- Modify: `core/src/lib.rs` (uncomment `pub mod adapters;`)

- [ ] **Step 1: Write synthetic fixture** `core/tests/fixtures/claude/basic_session.jsonl`

```jsonl
{"type":"system","subtype":"init","session_id":"abc123","model":"claude-fable-5","tools":[]}
{"type":"assistant","message":{"id":"msg_01","role":"assistant","content":[{"type":"text","text":"ok"}]},"session_id":"abc123"}
{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"abc123","usage":{"input_tokens":42,"output_tokens":5}}
```

- [ ] **Step 2: Write `core/src/adapters/mod.rs`** (trait + RunRequest)

```rust
pub mod claude;
// pub mod codex;   // Task 5
// pub mod gemini;  // Task 6

use crate::event::{AgentEvent, Provider};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub cwd: PathBuf,
}

/// An adapter knows how to launch one provider's CLI and translate its raw
/// output into AgentEvents. Parsing is PURE (no I/O) so it is fixture-testable.
pub trait Adapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn cli_binary(&self) -> &'static str;
    fn build_command(&self, req: &RunRequest) -> tokio::process::Command;
    /// Streaming providers: one stdout line → zero or more events.
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        let _ = line;
        Vec::new()
    }
    /// Non-streaming providers: full stdout at process exit → events.
    fn parse_final(&self, full_output: &str) -> Vec<AgentEvent> {
        let _ = full_output;
        Vec::new()
    }
}
```

- [ ] **Step 3: Write failing tests** (bottom of `core/src/adapters/claude.rs`; create the file with only this tests module for now plus `use super::*;` stub — it will not compile, that's the RED step)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AgentEvent, Provider};

    const FIXTURE: &str = include_str!("../../tests/fixtures/claude/basic_session.jsonl");

    fn parse_all(raw: &str) -> Vec<AgentEvent> {
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .flat_map(|l| ClaudeAdapter.parse_line(l))
            .collect()
    }

    #[test]
    fn parses_basic_session_fixture() {
        let events = parse_all(FIXTURE);
        assert!(
            matches!(&events[0], AgentEvent::SessionStarted { provider: Provider::Claude, session_id, .. } if session_id == "abc123")
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Message { text } if text == "ok")));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { input_tokens: 42, output_tokens: 5 })));
        assert!(matches!(events.last().unwrap(), AgentEvent::Completed { result: Some(r) } if r == "ok"));
    }

    #[test]
    fn error_result_maps_to_failed() {
        let line = r#"{"type":"result","subtype":"error","is_error":true,"result":"limit reached","session_id":"abc123"}"#;
        let events = ClaudeAdapter.parse_line(line);
        assert!(matches!(&events[0], AgentEvent::Failed { error } if error == "limit reached"));
    }

    #[test]
    fn garbage_line_yields_no_events() {
        assert!(ClaudeAdapter.parse_line("not json at all").is_empty());
        assert!(ClaudeAdapter.parse_line(r#"{"type":"unknown_kind"}"#).is_empty());
    }

    #[test]
    fn build_command_uses_stream_json_and_model() {
        let req = RunRequest {
            prompt: "hi".into(),
            model: Some("sonnet".into()),
            cwd: std::env::temp_dir(),
        };
        let cmd = ClaudeAdapter.build_command(&req);
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"sonnet".to_string()));
    }
}
```

- [ ] **Step 4: Run, verify failure**

Run: `cargo test -p consilium claude`
Expected: COMPILE ERROR (`ClaudeAdapter` not defined)

- [ ] **Step 5: Implement** (top of `core/src/adapters/claude.rs`)

```rust
use super::{Adapter, RunRequest};
use crate::event::{AgentEvent, Provider};
use tokio::process::Command;

pub struct ClaudeAdapter;

impl Adapter for ClaudeAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }

    fn cli_binary(&self) -> &'static str {
        "claude"
    }

    fn build_command(&self, req: &RunRequest) -> Command {
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
            .arg(&req.prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose");
        if let Some(model) = &req.model {
            cmd.arg("--model").arg(model);
        }
        cmd.current_dir(&req.cwd);
        cmd
    }

    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return Vec::new();
        };
        match v["type"].as_str() {
            Some("system") if v["subtype"] == "init" => vec![AgentEvent::SessionStarted {
                session_id: v["session_id"].as_str().unwrap_or_default().to_string(),
                provider: Provider::Claude,
                model: v["model"].as_str().map(String::from),
            }],
            Some("assistant") => {
                let mut events = Vec::new();
                if let Some(blocks) = v["message"]["content"].as_array() {
                    for block in blocks {
                        match block["type"].as_str() {
                            Some("text") => events.push(AgentEvent::Message {
                                text: block["text"].as_str().unwrap_or_default().to_string(),
                            }),
                            Some("thinking") => events.push(AgentEvent::Thinking {
                                text: block["thinking"].as_str().unwrap_or_default().to_string(),
                            }),
                            Some("tool_use") => events.push(AgentEvent::ToolCall {
                                name: block["name"].as_str().unwrap_or_default().to_string(),
                                detail: block["input"].to_string(),
                            }),
                            _ => {}
                        }
                    }
                }
                events
            }
            Some("result") => {
                let mut events = Vec::new();
                if let Some(u) = v.get("usage") {
                    events.push(AgentEvent::Usage {
                        input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                    });
                }
                if v["is_error"].as_bool().unwrap_or(false) {
                    events.push(AgentEvent::Failed {
                        error: v["result"].as_str().unwrap_or("unknown error").to_string(),
                    });
                } else {
                    events.push(AgentEvent::Completed {
                        result: v["result"].as_str().map(String::from),
                    });
                }
                events
            }
            _ => Vec::new(),
        }
    }
}
```

- [ ] **Step 6: Uncomment `pub mod adapters;` in `core/src/lib.rs`, run tests**

Run: `cargo test -p consilium claude`
Expected: 4 passed

- [ ] **Step 7: Write `script/record_fixtures.sh`** (real-format verification; `chmod +x`)

```bash
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
```

- [ ] **Step 8: Add recorded-fixture test** (append to tests module in `core/src/adapters/claude.rs`)

```rust
    /// Runs only when real fixtures have been recorded via script/record_fixtures.sh.
    #[test]
    fn parses_recorded_real_output_if_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/claude/recorded/basic.jsonl"
        );
        let Ok(raw) = std::fs::read_to_string(path) else {
            eprintln!("skipped: no recorded fixture");
            return;
        };
        let events = parse_all(&raw);
        assert!(matches!(events.first(), Some(AgentEvent::SessionStarted { .. })));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Completed { .. })));
    }
```

- [ ] **Step 9: Record real claude fixture and verify parser against reality**

Run: `bash script/record_fixtures.sh` (claude will succeed; codex expected to FAIL until Task 9 installs it; gemini may need flag fix)
Then: `cargo test -p consilium claude`
Expected: all pass including `parses_recorded_real_output_if_present`. **If it fails: the real format drifted — update parser AND synthetic fixture to match recorded reality, re-run.**

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "feat: Adapter trait + Claude adapter with fixture tests"
```

### Task 5: Codex adapter (TDD, fixtures)

**Files:**
- Create: `core/src/adapters/codex.rs`
- Create: `core/tests/fixtures/codex/basic_session.jsonl`
- Modify: `core/src/adapters/mod.rs` (uncomment `pub mod codex;`)

- [ ] **Step 1: Write synthetic fixture** `core/tests/fixtures/codex/basic_session.jsonl`

```jsonl
{"type":"thread.started","thread_id":"th_1"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"command_execution","command":"ls","status":"completed"}}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"ok"}}
{"type":"turn.completed","usage":{"input_tokens":40,"cached_input_tokens":0,"output_tokens":6}}
```

- [ ] **Step 2: Write failing tests** (bottom of `core/src/adapters/codex.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AgentEvent, Provider};

    const FIXTURE: &str = include_str!("../../tests/fixtures/codex/basic_session.jsonl");

    fn parse_all(raw: &str) -> Vec<AgentEvent> {
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .flat_map(|l| CodexAdapter.parse_line(l))
            .collect()
    }

    #[test]
    fn parses_basic_session_fixture() {
        let events = parse_all(FIXTURE);
        assert!(
            matches!(&events[0], AgentEvent::SessionStarted { provider: Provider::Codex, session_id, .. } if session_id == "th_1")
        );
        assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolCall { name, .. } if name == "command_execution")));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Message { text } if text == "ok")));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { input_tokens: 40, output_tokens: 6 })));
        assert!(matches!(events.last().unwrap(), AgentEvent::Completed { .. }));
    }

    #[test]
    fn garbage_line_yields_no_events() {
        assert!(CodexAdapter.parse_line("???").is_empty());
    }

    #[test]
    fn build_command_uses_exec_json() {
        let req = RunRequest { prompt: "hi".into(), model: Some("gpt-5.4".into()), cwd: std::env::temp_dir() };
        let cmd = CodexAdapter.build_command(&req);
        let args: Vec<String> = cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args[0], "exec");
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"gpt-5.4".to_string()));
    }

    /// Runs only when real fixtures have been recorded via script/record_fixtures.sh.
    #[test]
    fn parses_recorded_real_output_if_present() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex/recorded/basic.jsonl");
        let Ok(raw) = std::fs::read_to_string(path) else {
            eprintln!("skipped: no recorded fixture");
            return;
        };
        let events = parse_all(&raw);
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Message { .. })));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Completed { .. })));
    }
}
```

- [ ] **Step 3: Run, verify failure**

Run: `cargo test -p consilium codex`
Expected: COMPILE ERROR

- [ ] **Step 4: Implement** (top of `core/src/adapters/codex.rs`)

```rust
use super::{Adapter, RunRequest};
use crate::event::{AgentEvent, Provider};
use tokio::process::Command;

pub struct CodexAdapter;

impl Adapter for CodexAdapter {
    fn provider(&self) -> Provider {
        Provider::Codex
    }

    fn cli_binary(&self) -> &'static str {
        "codex"
    }

    fn build_command(&self, req: &RunRequest) -> Command {
        let mut cmd = Command::new("codex");
        cmd.arg("exec").arg("--json");
        if let Some(model) = &req.model {
            cmd.arg("-m").arg(model);
        }
        cmd.arg(&req.prompt);
        cmd.current_dir(&req.cwd);
        cmd
    }

    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return Vec::new();
        };
        match v["type"].as_str() {
            Some("thread.started") => vec![AgentEvent::SessionStarted {
                session_id: v["thread_id"].as_str().unwrap_or_default().to_string(),
                provider: Provider::Codex,
                model: None,
            }],
            Some("item.completed") => {
                let item = &v["item"];
                match item["type"].as_str() {
                    Some("agent_message") => vec![AgentEvent::Message {
                        text: item["text"].as_str().unwrap_or_default().to_string(),
                    }],
                    Some("reasoning") => vec![AgentEvent::Thinking {
                        text: item["text"].as_str().unwrap_or_default().to_string(),
                    }],
                    Some(other) => vec![AgentEvent::ToolCall {
                        name: other.to_string(),
                        detail: item.to_string(),
                    }],
                    None => Vec::new(),
                }
            }
            Some("turn.completed") => {
                let mut events = Vec::new();
                if let Some(u) = v.get("usage") {
                    events.push(AgentEvent::Usage {
                        input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                    });
                }
                events.push(AgentEvent::Completed { result: None });
                events
            }
            Some("turn.failed") | Some("error") => vec![AgentEvent::Failed {
                error: v["error"]["message"]
                    .as_str()
                    .or(v["message"].as_str())
                    .unwrap_or("unknown error")
                    .to_string(),
            }],
            _ => Vec::new(),
        }
    }
}
```

- [ ] **Step 5: Uncomment `pub mod codex;`, run tests**

Run: `cargo test -p consilium codex`
Expected: 4 passed (recorded-fixture test self-skips until codex installed in Task 9)

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: Codex adapter with fixture tests"
```

### Task 6: Gemini adapter (TDD, fixtures)

**Files:**
- Create: `core/src/adapters/gemini.rs`
- Create: `core/tests/fixtures/gemini/basic_response.json`
- Modify: `core/src/adapters/mod.rs` (uncomment `pub mod gemini;`)

- [ ] **Step 1: Write synthetic fixture** `core/tests/fixtures/gemini/basic_response.json`

```json
{
  "response": "ok",
  "stats": {
    "models": {
      "gemini-3-pro": {
        "tokens": { "prompt": 12, "candidates": 3, "total": 15 }
      }
    }
  }
}
```

- [ ] **Step 2: Write failing tests** (bottom of `core/src/adapters/gemini.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AgentEvent;

    const FIXTURE: &str = include_str!("../../tests/fixtures/gemini/basic_response.json");

    #[test]
    fn parses_json_response_fixture() {
        let events = GeminiAdapter.parse_final(FIXTURE);
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Message { text } if text == "ok")));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { input_tokens: 12, output_tokens: 3 })));
        assert!(matches!(events.last().unwrap(), AgentEvent::Completed { result: Some(r) } if r == "ok"));
    }

    #[test]
    fn plain_text_output_falls_back_to_message() {
        let events = GeminiAdapter.parse_final("just plain text\n");
        assert!(matches!(&events[0], AgentEvent::Message { text } if text == "just plain text"));
        assert!(matches!(events.last().unwrap(), AgentEvent::Completed { .. }));
    }

    #[test]
    fn empty_output_yields_failed() {
        let events = GeminiAdapter.parse_final("   \n");
        assert!(matches!(&events[0], AgentEvent::Failed { .. }));
    }

    #[test]
    fn build_command_uses_json_output() {
        let req = RunRequest { prompt: "hi".into(), model: Some("gemini-3-pro".into()), cwd: std::env::temp_dir() };
        let cmd = GeminiAdapter.build_command(&req);
        let args: Vec<String> = cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"json".to_string()));
        assert!(args.contains(&"-m".to_string()));
    }

    /// Runs only when real fixtures have been recorded via script/record_fixtures.sh.
    #[test]
    fn parses_recorded_real_output_if_present() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gemini/recorded/basic.json");
        let Ok(raw) = std::fs::read_to_string(path) else {
            eprintln!("skipped: no recorded fixture");
            return;
        };
        let events = GeminiAdapter.parse_final(&raw);
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Message { .. })));
        assert!(matches!(events.last().unwrap(), AgentEvent::Completed { .. }));
    }
}
```

- [ ] **Step 3: Run, verify failure**

Run: `cargo test -p consilium gemini`
Expected: COMPILE ERROR

- [ ] **Step 4: Implement** (top of `core/src/adapters/gemini.rs`)

```rust
use super::{Adapter, RunRequest};
use crate::event::{AgentEvent, Provider};
use tokio::process::Command;

pub struct GeminiAdapter;

/// Best-effort extraction of token usage from gemini's stats blob: finds the
/// first per-model "tokens" object. Usage is optional — absence is not an error.
fn extract_usage(stats: &serde_json::Value) -> Option<AgentEvent> {
    let models = stats.get("models")?.as_object()?;
    let (_, first) = models.iter().next()?;
    let tokens = first.get("tokens")?;
    Some(AgentEvent::Usage {
        input_tokens: tokens["prompt"].as_u64().unwrap_or(0),
        output_tokens: tokens["candidates"].as_u64().unwrap_or(0),
    })
}

impl Adapter for GeminiAdapter {
    fn provider(&self) -> Provider {
        Provider::Gemini
    }

    fn cli_binary(&self) -> &'static str {
        "gemini"
    }

    fn build_command(&self, req: &RunRequest) -> Command {
        let mut cmd = Command::new("gemini");
        cmd.arg("-p").arg(&req.prompt).arg("--output-format").arg("json");
        if let Some(model) = &req.model {
            cmd.arg("-m").arg(model);
        }
        cmd.current_dir(&req.cwd);
        cmd
    }

    fn parse_final(&self, full_output: &str) -> Vec<AgentEvent> {
        let trimmed = full_output.trim();
        if trimmed.is_empty() {
            return vec![AgentEvent::Failed { error: "gemini produced no output".into() }];
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(response) = v["response"].as_str() {
                let mut events = vec![AgentEvent::Message { text: response.to_string() }];
                if let Some(usage) = v.get("stats").and_then(extract_usage) {
                    events.push(usage);
                }
                events.push(AgentEvent::Completed { result: Some(response.to_string()) });
                return events;
            }
        }
        // Plain-text fallback (older CLI versions or missing --output-format support)
        vec![
            AgentEvent::Message { text: trimmed.to_string() },
            AgentEvent::Completed { result: Some(trimmed.to_string()) },
        ]
    }
}
```

- [ ] **Step 5: Uncomment `pub mod gemini;`, run tests**

Run: `cargo test -p consilium gemini`
Expected: 5 passed

- [ ] **Step 6: Record real gemini fixture, verify against reality**

Run: `bash script/record_fixtures.sh && cargo test -p consilium gemini`
Expected: all pass. If the recorded format differs (flag name, stats shape) — fix `build_command`/`parse_final` and the synthetic fixture to match reality.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat: Gemini adapter with fixture tests"
```

### Task 7: SessionManager — spawn and stream (TDD via fake CLI)

**Files:**
- Create: `core/src/sessions.rs`
- Create: `core/tests/sessions_test.rs`
- Create: `core/tests/fake_cli_output.jsonl` (copy of claude fixture content)
- Modify: `core/src/lib.rs` (uncomment `pub mod sessions;`)

- [ ] **Step 1: Create `core/tests/fake_cli_output.jsonl`** — same 3 lines as `core/tests/fixtures/claude/basic_session.jsonl` (copy the file).

- [ ] **Step 2: Write failing integration test** `core/tests/sessions_test.rs`

```rust
use consilium::adapters::{Adapter, RunRequest};
use consilium::event::{AgentEvent, Provider};
use consilium::sessions;
use std::sync::Arc;

/// Fake adapter: "CLI" is `cat <fixture>`, parsing delegates to the Claude parser.
struct FakeAdapter {
    fixture: std::path::PathBuf,
}

impl Adapter for FakeAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }
    fn cli_binary(&self) -> &'static str {
        "cat"
    }
    fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("cat");
        cmd.arg(&self.fixture);
        cmd
    }
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        consilium::adapters::claude::ClaudeAdapter.parse_line(line)
    }
}

/// Fake adapter whose process exits non-zero without output.
struct CrashingAdapter;

impl Adapter for CrashingAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg("exit 3");
        cmd
    }
}

fn req() -> RunRequest {
    RunRequest { prompt: "hi".into(), model: None, cwd: std::env::temp_dir() }
}

async fn collect(mut handle: sessions::SessionHandle) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(ev) = handle.events.recv().await {
        events.push(ev);
    }
    events
}

#[tokio::test]
async fn streams_events_from_process_in_order() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_cli_output.jsonl");
    let handle = sessions::spawn(Arc::new(FakeAdapter { fixture }), req()).unwrap();
    let events = collect(handle).await;
    assert!(matches!(events.first(), Some(AgentEvent::SessionStarted { .. })));
    assert!(matches!(events.last(), Some(AgentEvent::Completed { .. })));
}

#[tokio::test]
async fn nonzero_exit_emits_failed_event() {
    let handle = sessions::spawn(Arc::new(CrashingAdapter), req()).unwrap();
    let events = collect(handle).await;
    assert!(matches!(events.last(), Some(AgentEvent::Failed { error }) if error.contains("3")));
}
```

- [ ] **Step 3: Run, verify failure**

Run: `cargo test -p consilium --test sessions_test`
Expected: COMPILE ERROR (`sessions` module missing)

- [ ] **Step 4: Implement `core/src/sessions.rs`**

```rust
use crate::adapters::{Adapter, RunRequest};
use crate::event::AgentEvent;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

pub struct SessionHandle {
    pub id: String,
    pub events: mpsc::Receiver<AgentEvent>,
}

fn next_session_id(provider: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_nanos();
    format!("{provider}-{nanos}")
}

/// Spawns the adapter's CLI process and streams normalized events.
/// The channel closes when the process exits and all events are delivered.
pub fn spawn(adapter: Arc<dyn Adapter>, req: RunRequest) -> anyhow::Result<SessionHandle> {
    let (tx, rx) = mpsc::channel::<AgentEvent>(256);
    let id = next_session_id(adapter.provider().as_str());

    let mut cmd = adapter.build_command(&req);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!("failed to spawn {}: {e}", adapter.cli_binary())
    })?;
    let stdout = child.stdout.take().expect("stdout piped above");

    tokio::spawn(async move {
        let mut full_output = String::new();
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            full_output.push_str(&line);
            full_output.push('\n');
            for ev in adapter.parse_line(&line) {
                if tx.send(ev).await.is_err() {
                    return; // receiver dropped — stop streaming
                }
            }
        }
        for ev in adapter.parse_final(&full_output) {
            if tx.send(ev).await.is_err() {
                return;
            }
        }
        match child.wait().await {
            Ok(status) if !status.success() => {
                let _ = tx
                    .send(AgentEvent::Failed {
                        error: format!("process exited with {status}"),
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx.send(AgentEvent::Failed { error: e.to_string() }).await;
            }
            _ => {}
        }
    });

    Ok(SessionHandle { id, events: rx })
}
```

Note: `FakeAdapter` in the test emits `Completed` via `parse_line` (claude fixture) *and* the process exits 0, so the last event is `Completed`. `CrashingAdapter` produces no parse events, so the last event is `Failed`. The double-`Completed` dedup problem (parse events + exit status) is deliberately NOT handled in M1 — orchestrator (M2) treats the first terminal event as authoritative.

- [ ] **Step 5: Uncomment `pub mod sessions;`, run tests**

Run: `cargo test -p consilium --test sessions_test`
Expected: 2 passed

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: SessionManager spawns CLIs and streams normalized events"
```

### Task 8: Quota store (TDD)

**Files:**
- Create: `core/src/quota.rs`
- Modify: `core/src/lib.rs` (uncomment `pub mod quota;`)

- [ ] **Step 1: Write failing tests** (bottom of `core/src/quota.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Provider;

    #[test]
    fn records_and_aggregates_usage() {
        let store = QuotaStore::open_in_memory().unwrap();
        store.record(Provider::Gemini, 100, 20).unwrap();
        store.record(Provider::Gemini, 50, 10).unwrap();
        store.record(Provider::Codex, 7, 3).unwrap();
        let (input, output) = store.totals_since(Provider::Gemini, 0).unwrap();
        assert_eq!((input, output), (150, 30));
        let (input, output) = store.totals_since(Provider::Codex, 0).unwrap();
        assert_eq!((input, output), (7, 3));
    }

    #[test]
    fn window_excludes_old_rows() {
        let store = QuotaStore::open_in_memory().unwrap();
        let now = unix_now();
        store.record_at(Provider::Claude, 1000, 500, now - 10_000).unwrap();
        store.record_at(Provider::Claude, 10, 5, now).unwrap();
        let (input, output) = store.totals_since(Provider::Claude, now - 3600).unwrap();
        assert_eq!((input, output), (10, 5));
    }

    #[test]
    fn empty_store_returns_zero() {
        let store = QuotaStore::open_in_memory().unwrap();
        assert_eq!(store.totals_since(Provider::Claude, 0).unwrap(), (0, 0));
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test -p consilium quota`
Expected: COMPILE ERROR

- [ ] **Step 3: Implement** (top of `core/src/quota.rs`)

```rust
use crate::event::Provider;
use rusqlite::Connection;
use std::path::Path;

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_secs() as i64
}

pub struct QuotaStore {
    conn: Connection,
}

impl QuotaStore {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS usage_log (
                id INTEGER PRIMARY KEY,
                ts INTEGER NOT NULL,
                provider TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL
            )",
            [],
        )?;
        Ok(Self { conn })
    }

    pub fn record(&self, provider: Provider, input_tokens: u64, output_tokens: u64) -> anyhow::Result<()> {
        self.record_at(provider, input_tokens, output_tokens, unix_now())
    }

    pub fn record_at(&self, provider: Provider, input_tokens: u64, output_tokens: u64, ts: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO usage_log (ts, provider, input_tokens, output_tokens) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![ts, provider.as_str(), input_tokens as i64, output_tokens as i64],
        )?;
        Ok(())
    }

    /// Sum of (input, output) tokens for a provider since the given unix timestamp.
    pub fn totals_since(&self, provider: Provider, since_unix: i64) -> anyhow::Result<(u64, u64)> {
        let (input, output): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0)
             FROM usage_log WHERE provider = ?1 AND ts >= ?2",
            rusqlite::params![provider.as_str(), since_unix],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((input as u64, output as u64))
    }
}
```

- [ ] **Step 4: Uncomment `pub mod quota;`, run tests**

Run: `cargo test -p consilium quota`
Expected: 3 passed

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: SQLite quota store with sliding-window aggregation"
```

### Task 9: Doctor command + codex installation

**Files:**
- Create: `core/src/doctor.rs`
- Modify: `core/src/lib.rs` (uncomment `pub mod doctor;`)
- Modify: `core/src/main.rs` (wire `Doctor`)

- [ ] **Step 1: Write failing tests** (bottom of `core/src/doctor.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn fake_bin_dir(name: &str, output: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho \"{output}\"\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    #[test]
    fn detects_installed_cli_and_version() {
        let dir = fake_bin_dir("fakecli", "fakecli 9.9.9");
        let status = check_with_path("fakecli", Some(dir.path().as_os_str()));
        assert!(status.found);
        assert_eq!(status.version.as_deref(), Some("fakecli 9.9.9"));
    }

    #[test]
    fn reports_missing_cli() {
        let dir = tempfile::tempdir().unwrap(); // empty dir on PATH
        let status = check_with_path("definitely-not-installed", Some(dir.path().as_os_str()));
        assert!(!status.found);
        assert!(status.version.is_none());
    }
}
```

Add dev-dependency to `core/Cargo.toml`:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test -p consilium doctor`
Expected: COMPILE ERROR

- [ ] **Step 3: Implement** (top of `core/src/doctor.rs`)

```rust
use std::ffi::OsStr;

pub struct CliStatus {
    pub binary: String,
    pub found: bool,
    pub version: Option<String>,
}

/// Checks `<binary> --version` resolving through PATH (or an override for tests).
pub fn check_with_path(binary: &str, path_override: Option<&OsStr>) -> CliStatus {
    let mut cmd = std::process::Command::new(binary);
    cmd.arg("--version");
    if let Some(path) = path_override {
        cmd.env("PATH", path);
    }
    match cmd.output() {
        Ok(out) if out.status.success() => CliStatus {
            binary: binary.to_string(),
            found: true,
            version: Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        },
        _ => CliStatus { binary: binary.to_string(), found: false, version: None },
    }
}

pub fn check(binary: &str) -> CliStatus {
    check_with_path(binary, None)
}

pub fn run_doctor() -> Vec<CliStatus> {
    ["claude", "codex", "gemini"].iter().map(|b| check(b)).collect()
}
```

- [ ] **Step 4: Wire into `core/src/main.rs`** — replace the `Command::Doctor` arm:

```rust
        Command::Doctor => {
            let mut all_ok = true;
            for status in consilium::doctor::run_doctor() {
                if status.found {
                    println!("✓ {:8} {}", status.binary, status.version.unwrap_or_default());
                } else {
                    all_ok = false;
                    println!("✗ {:8} not found", status.binary);
                }
            }
            if !all_ok {
                println!("\nInstall missing CLIs:");
                println!("  codex:  npm install -g @openai/codex   (then: codex login)");
                println!("  gemini: npm install -g @google/gemini-cli");
                println!("  claude: see https://code.claude.com");
                std::process::exit(1);
            }
        }
```

- [ ] **Step 5: Run tests + smoke**

Run: `cargo test -p consilium doctor && cargo run -- doctor`
Expected: tests pass; doctor output shows ✓ claude, ✗ codex (not installed yet), ✓ gemini; exit code 1

- [ ] **Step 6: Install codex CLI for real** (user's machine; requires ChatGPT subscription login)

```bash
npm install -g @openai/codex && codex --version
```

Then ask the user to run `codex login` interactively (browser auth — cannot be automated).
After login: `cargo run -- doctor` → all three ✓, exit 0.

- [ ] **Step 7: Record codex fixture now that it's installed**

Run: `bash script/record_fixtures.sh && cargo test -p consilium codex`
Expected: `parses_recorded_real_output_if_present` now exercises real output. Fix parser/fixture if format drifted.

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "feat: doctor command checks agent CLIs"
```

### Task 10: Run + quota commands (end-to-end wiring)

**Files:**
- Modify: `core/src/main.rs` (wire `Run` and `Quota`)

- [ ] **Step 1: Implement the `Run` arm** in `core/src/main.rs`:

```rust
        Command::Run { provider, model, prompt } => {
            use consilium::adapters::{claude::ClaudeAdapter, codex::CodexAdapter, gemini::GeminiAdapter, Adapter, RunRequest};
            use consilium::event::{AgentEvent, Provider};
            use std::sync::Arc;

            let provider: Provider = provider.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let adapter: Arc<dyn Adapter> = match provider {
                Provider::Claude => Arc::new(ClaudeAdapter),
                Provider::Codex => Arc::new(CodexAdapter),
                Provider::Gemini => Arc::new(GeminiAdapter),
            };
            let req = RunRequest { prompt, model, cwd: std::env::current_dir()? };
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;

            let mut handle = consilium::sessions::spawn(adapter, req)?;
            println!("session: {}", handle.id);
            while let Some(ev) = handle.events.recv().await {
                match &ev {
                    AgentEvent::Usage { input_tokens, output_tokens } => {
                        store.record(provider, *input_tokens, *output_tokens)?;
                        println!("[usage] in={input_tokens} out={output_tokens}");
                    }
                    AgentEvent::Message { text } => println!("[message] {text}"),
                    AgentEvent::ToolCall { name, .. } => println!("[tool] {name}"),
                    AgentEvent::Completed { .. } => println!("[completed]"),
                    AgentEvent::Failed { error } => println!("[failed] {error}"),
                    other => println!("[event] {other:?}"),
                }
            }
        }
```

And the `Quota` arm plus a helper (place the helper above `main`):

```rust
fn quota_db_path() -> anyhow::Result<std::path::PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(std::path::PathBuf::from(home).join(".consilium").join("usage.db"))
}
```

```rust
        Command::Quota => {
            use consilium::event::Provider;
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let since = consilium::quota::unix_now() - 5 * 3600;
            println!("usage in the last 5h window:");
            for p in [Provider::Claude, Provider::Codex, Provider::Gemini] {
                let (input, output) = store.totals_since(p, since)?;
                println!("  {:8} in={input:>8} out={output:>8}", p.as_str());
            }
        }
```

- [ ] **Step 2: Full test suite + lints**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean format, no clippy warnings, all tests pass

- [ ] **Step 3: Real end-to-end smoke (spends 1 gemini request — cheapest pool)**

Run: `cargo run -- run --provider gemini "Reply with exactly: ok"`
Expected: `[message] ok`, `[usage] ...`, `[completed]`
Then: `cargo run -- quota`
Expected: gemini row shows non-zero tokens

- [ ] **Step 4: Commit milestone**

```bash
git add -A && git commit -m "feat: run and quota commands — M1 engine foundation complete"
```

---

## M1 Exit Criteria

- `cargo test` green, `cargo clippy -- -D warnings` clean.
- `consilium doctor` shows ✓ for claude, codex, gemini (codex installed + logged in).
- `consilium run --provider gemini "..."` streams normalized events end-to-end and records usage.
- Real recorded fixtures exist for all three providers; parsers verified against them.

## Next plans (written after M1 ships)

- **M2:** orchestration primitives (council/conduct/review), auto pipeline, supervisor.
- **M3:** axum server (REST+WS), MCP attached mode (rmcp), React web UI, quota dashboards.
