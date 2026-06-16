use crate::adapters::{
    claude::ClaudeAdapter, codex::CodexAdapter, gemini::GeminiAdapter, Adapter, RunRequest,
};
use crate::config::RoleConfig;
use crate::event::Provider;
use crate::orchestrator::resilience::Rung;
use std::path::PathBuf;
use std::sync::Arc;

/// Returns the adapter for a given provider.
fn adapter_for_provider(p: Provider) -> Arc<dyn Adapter> {
    match p {
        Provider::Claude => Arc::new(ClaudeAdapter),
        Provider::Codex => Arc::new(CodexAdapter),
        Provider::Gemini => Arc::new(GeminiAdapter),
    }
}

pub fn adapter_for(role: &RoleConfig) -> Arc<dyn Adapter> {
    adapter_for_provider(role.provider)
}

/// Resolves a role config into its failover ladder: one Rung (candidate +
/// adapter) per ladder entry, primary first.
pub fn resolve_ladder(role: &RoleConfig) -> Vec<Rung> {
    role.ladder()
        .into_iter()
        .map(|candidate| {
            let adapter = adapter_for_provider(candidate.provider);
            Rung { candidate, adapter }
        })
        .collect()
}

/// Builds the RunRequest for a role. Primary consumer: M2b conduct/supervisor
/// (council/review inline their requests). `effort` is intentionally NOT applied yet:
/// per-CLI effort flags are unverified — TODO(M2b): map after checking real CLIs.
pub fn request_for(role: &RoleConfig, prompt: String, cwd: PathBuf) -> RunRequest {
    RunRequest {
        prompt,
        model: Some(role.model.clone()),
        cwd,
        // Execution-oriented default: provider safeguards stay armed. Advisory
        // callers (council/review) build their requests directly.
        advisory: false,
        write: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoleConfig;
    use crate::event::Provider;

    fn role(provider: Provider, model: &str) -> RoleConfig {
        serde_json::from_value(serde_json::json!({
            "provider": provider.as_str(),
            "model": model
        }))
        .unwrap()
    }

    #[test]
    fn adapter_for_maps_each_provider() {
        assert_eq!(
            adapter_for(&role(Provider::Claude, "sonnet")).provider(),
            Provider::Claude
        );
        assert_eq!(
            adapter_for(&role(Provider::Codex, "gpt-5.4")).provider(),
            Provider::Codex
        );
        assert_eq!(
            adapter_for(&role(Provider::Gemini, "gemini-3-pro")).provider(),
            Provider::Gemini
        );
    }

    #[test]
    fn request_for_carries_model_and_prompt() {
        let r = request_for(
            &role(Provider::Codex, "gpt-5.4"),
            "do it".into(),
            std::env::temp_dir(),
        );
        assert_eq!(r.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(r.prompt, "do it");
    }

    #[test]
    fn resolves_role_to_a_rung_per_ladder_entry() {
        let role_cfg: RoleConfig = serde_json::from_value(serde_json::json!({
            "provider": "claude", "model": "claude-opus-4-8",
            "fallbacks": [{"provider": "codex", "model": "gpt-5.4"}]
        }))
        .unwrap();
        let ladder = resolve_ladder(&role_cfg);
        assert_eq!(ladder.len(), 2);
        assert_eq!(ladder[0].candidate.provider, Provider::Claude);
        assert_eq!(ladder[0].adapter.provider(), Provider::Claude);
        assert_eq!(ladder[1].adapter.provider(), Provider::Codex);
    }
}
