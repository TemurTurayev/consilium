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
        Self {
            provider,
            model: model.into(),
            effort: None,
            mode: None,
            intervention_threshold: None,
        }
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
        let Some(path) = path else {
            return Ok(Config::default());
        };
        if !path.exists() {
            return Ok(Config::default());
        }
        let raw = std::fs::read_to_string(path)?;
        let cfg = serde_json::from_str(&raw)?;
        Ok(cfg)
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
}
