use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub enum Provider {
    Claude,
    Codex,
    Gemini,
    /// xAI's Grok Build CLI (`grok`) — added as a beta provider. Its headless
    /// NDJSON event schema is unstable (see `adapters::grok`), so treat this
    /// provider's token accounting as estimated until real fixtures land.
    Grok,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Gemini => "gemini",
            Provider::Grok => "grok",
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
            "grok" => Ok(Provider::Grok),
            other => Err(format!("unknown provider: {other}")),
        }
    }
}

/// Normalized event stream — the contract every adapter maps its CLI output into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub enum AgentEvent {
    SessionStarted {
        session_id: String,
        provider: Provider,
        model: Option<String>,
    },
    Thinking {
        text: String,
    },
    Message {
        text: String,
    },
    ToolCall {
        name: String,
        detail: String,
    },
    FileChanged {
        path: String,
    },
    Usage {
        #[ts(type = "number")]
        input_tokens: u64,
        #[ts(type = "number")]
        output_tokens: u64,
    },
    Completed {
        result: Option<String>,
    },
    Failed {
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_parses_from_str() {
        assert_eq!("claude".parse::<Provider>().unwrap(), Provider::Claude);
        assert_eq!("codex".parse::<Provider>().unwrap(), Provider::Codex);
        assert_eq!("gemini".parse::<Provider>().unwrap(), Provider::Gemini);
        assert_eq!("grok".parse::<Provider>().unwrap(), Provider::Grok);
        assert!("warp".parse::<Provider>().is_err());
    }

    #[test]
    fn agent_event_serializes_with_snake_case_tag() {
        let ev = AgentEvent::Usage {
            input_tokens: 10,
            output_tokens: 2,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"type":"usage","input_tokens":10,"output_tokens":2}"#
        );
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

    // serde serializes u64 as a bare JSON number; ts-rs's default is `bigint`,
    // which `JSON.parse` never yields. `#[ts(type = "number")]` keeps the TS
    // binding honest. This guard fails the build if that override is dropped.
    #[test]
    fn ts_usage_tokens_are_number_not_bigint() {
        use ts_rs::TS;
        let decl = AgentEvent::decl(&Default::default());
        assert!(
            decl.contains("input_tokens: number"),
            "u64 must map to TS number: {decl}"
        );
        assert!(!decl.contains("bigint"), "no bigint on the wire: {decl}");
    }
}
