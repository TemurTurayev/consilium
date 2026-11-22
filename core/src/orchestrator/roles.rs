use crate::adapters::{
    claude::ClaudeAdapter, codex::CodexAdapter, gemini::GeminiAdapter, grok::GrokAdapter, Adapter,
    RunRequest,
};
use crate::config::RoleConfig;
use crate::event::Provider;
use crate::orchestrator::council::CouncilMember;
use crate::orchestrator::resilience::Rung;
use std::path::PathBuf;
use std::sync::Arc;

/// Returns the adapter for a given provider.
fn adapter_for_provider(p: Provider) -> Arc<dyn Adapter> {
    match p {
        Provider::Claude => Arc::new(ClaudeAdapter),
        Provider::Codex => Arc::new(CodexAdapter),
        Provider::Gemini => Arc::new(GeminiAdapter),
        Provider::Grok => Arc::new(GrokAdapter),
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

/// The configured workers as addressable council members: each with its
/// failover ladder and a label, `provider-model`, that names it — to the
/// conductor routing work (`run_worker`) and in every report. A label that
/// would repeat an earlier worker's gets a `#2`, `#3`… suffix, so two workers
/// on one model can both be reached.
pub fn resolve_workers(roles: &[RoleConfig]) -> Vec<CouncilMember> {
    roles
        .iter()
        .enumerate()
        .map(|(index, role)| {
            let name = worker_name(role);
            let earlier = roles[..index]
                .iter()
                .filter(|other| worker_name(other) == name)
                .count();
            CouncilMember {
                label: match earlier {
                    0 => name,
                    n => format!("{name}#{}", n + 1),
                },
                ladder: resolve_ladder(role),
            }
        })
        .collect()
}

fn worker_name(role: &RoleConfig) -> String {
    format!("{}-{}", role.provider.as_str(), role.model)
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
        assert_eq!(
            adapter_for(&role(Provider::Grok, "grok-build")).provider(),
            Provider::Grok
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

    fn labels(roles: &[RoleConfig]) -> Vec<String> {
        resolve_workers(roles)
            .into_iter()
            .map(|w| w.label)
            .collect()
    }

    #[test]
    fn a_worker_is_labelled_by_its_provider_and_model() {
        let workers = resolve_workers(&[
            role(Provider::Codex, "gpt-6-luna"),
            role(Provider::Gemini, "claude-opus-5-5-high"),
        ]);
        assert_eq!(workers[0].label, "codex-gpt-6-luna");
        assert_eq!(workers[1].label, "gemini-claude-opus-5-5-high");
        assert_eq!(workers[1].ladder.len(), 1);
        assert_eq!(workers[1].ladder[0].candidate.provider, Provider::Gemini);
    }

    /// The label is how a worker is addressed. Two workers on one model shared
    /// a label, and the second could never be reached.
    #[test]
    fn workers_on_the_same_model_get_distinct_labels() {
        let same = role(Provider::Gemini, "gemini-4-argon-high");
        assert_eq!(
            labels(&[
                same.clone(),
                role(Provider::Codex, "gpt"),
                same.clone(),
                same
            ]),
            [
                "gemini-gemini-4-argon-high",
                "codex-gpt",
                "gemini-gemini-4-argon-high#2",
                "gemini-gemini-4-argon-high#3",
            ]
        );
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
