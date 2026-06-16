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
                    RoleConfig::new(Provider::Codex, "gpt-5.4"),
                    RoleConfig::new(Provider::Gemini, "gemini-3-pro-preview"),
                ],
                reviewer: RoleConfig::new(Provider::Codex, "gpt-5.4"),
                supervisor: RoleConfig {
                    intervention_threshold: Some("medium".into()),
                    ..RoleConfig::new(Provider::Gemini, "gemini-3-pro-preview")
                },
            },
            quota: QuotaConfig::default(),
        }
    }
}

impl Config {
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
}
