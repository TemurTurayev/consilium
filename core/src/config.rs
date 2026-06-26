use crate::event::Provider;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One rung of a role's failover ladder: a concrete (provider, model) pair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelCandidate {
    pub provider: Provider,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoleConfig {
    pub provider: Provider,
    pub model: String,
    /// Ordered failover candidates tried after the primary (provider, model)
    /// when it is unavailable or rate-limited. Empty = no failover.
    #[serde(default)]
    pub fallbacks: Vec<ModelCandidate>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub intervention_threshold: Option<String>,
}

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

/// Explicit build/test/lint commands for grounding conduct's accept/rework.
/// Any field None falls back to ecosystem auto-detection; if neither yields a
/// command, that check is skipped (recorded as "did not run").
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyConfig {
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub test: Option<String>,
    #[serde(default)]
    pub lint: Option<String>,
}

/// Conductor working memory: a live plan ledger (prior subtasks' status) plus the
/// current subtask's cumulative attempt history, injected as XML-isolated prompt
/// text into the conductor-facing stages (evaluation / supervisor / arbiter) and
/// the rework prompt (history only). Each block is bounded by a char cap so cost
/// stays bounded on long multi-subtask runs. Default ON — the conductor remembers
/// out of the box; empty blocks are elided, so cost is paid only where there is
/// real prior context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConductorMemoryConfig {
    #[serde(default = "default_memory_enabled")]
    pub enabled: bool,
    /// Max chars of the rendered <plan_ledger> block per call.
    #[serde(default = "default_ledger_char_cap")]
    pub ledger_char_cap: usize,
    /// Max chars of the rendered <attempt_history> block per call.
    #[serde(default = "default_attempt_history_char_cap")]
    pub attempt_history_char_cap: usize,
}

fn default_memory_enabled() -> bool {
    true
}
fn default_ledger_char_cap() -> usize {
    1500
}
fn default_attempt_history_char_cap() -> usize {
    800
}

impl Default for ConductorMemoryConfig {
    fn default() -> Self {
        Self {
            enabled: default_memory_enabled(),
            ledger_char_cap: default_ledger_char_cap(),
            attempt_history_char_cap: default_attempt_history_char_cap(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub roles: RolesConfig,
    #[serde(default)]
    pub quota: QuotaConfig,
    #[serde(default)]
    pub verify: Option<VerifyConfig>,
    /// Conductor working memory. `None` in a user file resolves to the default
    /// (enabled) at the call site via `unwrap_or_default`; `consilium init`
    /// emits it explicitly so the knob is discoverable.
    #[serde(default)]
    pub conductor_memory: Option<ConductorMemoryConfig>,
    /// Cross-family review (Finding 7): route a subtask's diff to a reviewer /
    /// arbiter of a DIFFERENT model family than the worker that produced it
    /// (kills self-preference bias). Default off — enabling it changes which
    /// model reviews on the stock config; flip on after validating in practice.
    #[serde(default)]
    pub cross_family_review: bool,
    /// Max times the conductor regenerates the plan after a subtask failure
    /// (M4 P1.4 replan). `0` = never replan (default).
    #[serde(default)]
    pub max_replans: u32,
    /// Optional total wall-clock budget for a conduct run, in seconds (M4 P1.6).
    /// `None` = unlimited (default); on overrun the run ships the subtasks done
    /// so far and reports the rest unfinished.
    #[serde(default)]
    pub budget_secs: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roles: RolesConfig {
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
                workers: vec![
                    RoleConfig::new(Provider::Codex, "gpt-5.5"),
                    RoleConfig::new(Provider::Gemini, "Gemini 3.1 Pro (High)"),
                ],
                reviewer: RoleConfig::new(Provider::Codex, "gpt-5.5"),
                supervisor: RoleConfig {
                    intervention_threshold: Some("medium".into()),
                    ..RoleConfig::new(Provider::Gemini, "Gemini 3.1 Pro (High)")
                },
            },
            quota: QuotaConfig::default(),
            verify: None,
            conductor_memory: Some(ConductorMemoryConfig::default()),
            cross_family_review: false,
            max_replans: 0,
            budget_secs: None,
        }
    }
}

impl Config {
    /// Serialize the config to pretty-printed JSON.
    pub fn to_pretty_json(&self) -> anyhow::Result<String> {
        serde_json::to_string_pretty(self).map_err(Into::into)
    }

    /// Load from path; missing file → defaults. Parse error → Err (never silently default).
    pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
        let Some(path) = path else {
            return Ok(Config::default());
        };
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
            Ok(raw) => serde_json::from_str(&raw).map_err(Into::into),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_when_file_missing() {
        let cfg = Config::load(Some(std::path::Path::new(
            "/nonexistent/consilium.config.json",
        )))
        .unwrap();
        assert_eq!(cfg.roles.conductor.provider, crate::event::Provider::Claude);
        assert!(!cfg.roles.workers.is_empty());
    }

    #[test]
    fn replan_and_budget_default_off_and_parse_camelcase() {
        let def = Config::default();
        assert_eq!(def.max_replans, 0, "replan off by default");
        assert_eq!(def.budget_secs, None, "no budget by default");

        let json = r#"{
          "roles": {
            "conductor":  { "provider": "claude", "model": "m" },
            "chairman":   { "provider": "claude", "model": "m" },
            "workers":    [{ "provider": "codex", "model": "g" }],
            "reviewer":   { "provider": "codex", "model": "g" },
            "supervisor": { "provider": "gemini", "model": "g" }
          },
          "maxReplans": 2,
          "budgetSecs": 600
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.max_replans, 2);
        assert_eq!(cfg.budget_secs, Some(600));
    }

    #[test]
    fn parses_spec_example() {
        let json = r#"{
          "roles": {
            "conductor":  { "provider": "claude", "model": "fable-5", "effort": "high", "mode": "attached" },
            "chairman":   { "provider": "claude", "model": "fable-5", "effort": "high" },
            "workers": [
              { "provider": "codex",  "model": "gpt-5.5" },
              { "provider": "gemini", "model": "gemini-3-pro" },
              { "provider": "claude", "model": "sonnet" }
            ],
            "reviewer":   { "provider": "codex",  "model": "gpt-5.5" },
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
        assert_eq!(
            cfg.roles.supervisor.intervention_threshold.as_deref(),
            Some("medium")
        );
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
                {"provider": "codex", "model": "gpt-5.5"}
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

    #[test]
    fn default_config_round_trips_through_json() {
        let original = Config::default();
        let json = original
            .to_pretty_json()
            .expect("serialization should succeed");
        let parsed: Config = serde_json::from_str(&json).expect("emitted JSON must be valid");
        let ladder = parsed.roles.conductor.ladder();
        assert_eq!(
            ladder.len(),
            2,
            "conductor ladder should have 2 rungs after round-trip"
        );
        assert_eq!(ladder[0].model, "claude-opus-4-8");
    }

    #[test]
    fn conductor_memory_defaults_to_enabled() {
        let m = ConductorMemoryConfig::default();
        assert!(m.enabled);
        assert_eq!(m.ledger_char_cap, 1500);
        assert_eq!(m.attempt_history_char_cap, 800);
        // Config default emits the block so `consilium init` writes it.
        assert!(Config::default().conductor_memory.is_some());
    }

    #[test]
    fn conductor_memory_parses_and_round_trips() {
        let json = r#"{"roles":{"conductor":{"provider":"claude","model":"m"},
            "chairman":{"provider":"claude","model":"m"},"workers":[],
            "reviewer":{"provider":"codex","model":"m"},
            "supervisor":{"provider":"gemini","model":"m"}},
            "conductorMemory":{"enabled":false,"ledgerCharCap":42,"attemptHistoryCharCap":7}}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        let m = cfg.conductor_memory.clone().unwrap();
        assert!(!m.enabled);
        assert_eq!(m.ledger_char_cap, 42);
        assert_eq!(m.attempt_history_char_cap, 7);
        // round-trip
        let back: Config = serde_json::from_str(&cfg.to_pretty_json().unwrap()).unwrap();
        assert_eq!(back.conductor_memory, cfg.conductor_memory);
    }

    #[test]
    fn conductor_memory_omitted_field_resolves_to_enabled_default() {
        // A user file that omits the block parses as None; consumers
        // `unwrap_or_default()` → enabled.
        let json = r#"{"roles":{"conductor":{"provider":"claude","model":"m"},
            "chairman":{"provider":"claude","model":"m"},"workers":[],
            "reviewer":{"provider":"codex","model":"m"},
            "supervisor":{"provider":"gemini","model":"m"}}}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert!(cfg.conductor_memory.is_none());
        assert!(cfg.conductor_memory.unwrap_or_default().enabled);
    }

    #[test]
    fn cross_family_review_defaults_off_and_parses() {
        assert!(!Config::default().cross_family_review);
        let json = r#"{"roles":{"conductor":{"provider":"claude","model":"m"},
            "chairman":{"provider":"claude","model":"m"},"workers":[],
            "reviewer":{"provider":"codex","model":"m"},
            "supervisor":{"provider":"gemini","model":"m"}},
            "crossFamilyReview":true}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert!(cfg.cross_family_review);
    }

    #[test]
    fn verify_config_parses() {
        let json = r#"{"roles":{"conductor":{"provider":"claude","model":"m"},
            "chairman":{"provider":"claude","model":"m"},"workers":[],
            "reviewer":{"provider":"codex","model":"m"},
            "supervisor":{"provider":"gemini","model":"m"}},
            "verify":{"test":"cargo test","build":"cargo build"}}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        let v = cfg.verify.unwrap();
        assert_eq!(v.test.as_deref(), Some("cargo test"));
        assert_eq!(v.build.as_deref(), Some("cargo build"));
        assert!(v.lint.is_none());
    }
}
