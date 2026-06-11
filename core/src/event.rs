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
        input_tokens: u64,
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
}
