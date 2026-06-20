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
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let version = if stdout.is_empty() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if stderr.is_empty() {
                    None
                } else {
                    Some(stderr)
                }
            } else {
                Some(stdout)
            };
            CliStatus {
                binary: binary.to_string(),
                found: true,
                version,
            }
        }
        _ => CliStatus {
            binary: binary.to_string(),
            found: false,
            version: None,
        },
    }
}

pub fn check(binary: &str) -> CliStatus {
    check_with_path(binary, None)
}

pub fn run_doctor() -> Vec<CliStatus> {
    ["claude", "codex", "gemini"]
        .iter()
        .map(|b| check(b))
        .collect()
}

// ── Model probing ──────────────────────────────────────────────────────────────

use crate::adapters::{Adapter, FailureKind, RunRequest};
use crate::config::{Config, ModelCandidate};
use crate::event::Provider;
use crate::orchestrator::runner::{run_to_completion, RunStatus};
use crate::quota::QuotaStore;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Result of probing one (provider, model) pair for liveness.
#[derive(Debug)]
pub struct ModelProbe {
    pub provider: Provider,
    pub model: String,
    pub ok: bool,
    /// Human-readable detail: "ok", "unavailable", "rate-limited", or "transient: …"
    pub detail: String,
}

/// Maps a `FailureKind` to a short human-readable status label.
///
/// This is a pure function — unit-testable without any I/O.
pub fn probe_label(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::ModelUnavailable => "unavailable",
        FailureKind::RateLimited => "rate-limited",
        FailureKind::Transient => "transient failure",
    }
}

/// Collects the distinct (provider, model) pairs across all role ladders in a
/// `Config`. Deduplication is done by (provider, model) string — if the same
/// model appears in multiple roles (e.g. conductor primary + chairman fallback)
/// it is only probed once.
///
/// This is a pure function — unit-testable without any I/O.
pub fn collect_distinct_model_pairs(config: &Config) -> Vec<ModelCandidate> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut pairs: Vec<ModelCandidate> = Vec::new();

    let all_roles: Vec<&crate::config::RoleConfig> = {
        let r = &config.roles;
        let mut v: Vec<&crate::config::RoleConfig> =
            vec![&r.conductor, &r.chairman, &r.reviewer, &r.supervisor];
        v.extend(r.workers.iter());
        v
    };

    for role in all_roles {
        for candidate in role.ladder() {
            let key = (
                candidate.provider.as_str().to_string(),
                candidate.model.clone(),
            );
            if seen.insert(key) {
                pairs.push(candidate);
            }
        }
    }
    pairs
}

/// Probes one (provider, model) pair by running a tiny "Reply with: ok" prompt
/// through the provider's adapter. Spends a small amount of quota on the
/// provider. This function is intentionally NOT unit-tested (it spawns a real
/// CLI); test the pure helpers (`probe_label`, `collect_distinct_model_pairs`)
/// instead.
pub async fn probe_model(adapter: Arc<dyn Adapter>, model: &str, quota: &QuotaStore) -> ModelProbe {
    let provider = adapter.provider();
    let req = RunRequest {
        prompt: "Reply with: ok".into(),
        model: Some(model.to_string()),
        cwd: std::env::temp_dir(),
        advisory: true,
        write: false,
    };
    let timeout = Duration::from_secs(30);
    match run_to_completion(adapter.clone(), req, quota, timeout).await {
        Ok(outcome) => match outcome.status {
            RunStatus::Completed => ModelProbe {
                provider,
                model: model.to_string(),
                ok: true,
                detail: "ok".to_string(),
            },
            RunStatus::Failed(ref e) => {
                let kind = adapter.classify_failure(e);
                ModelProbe {
                    provider,
                    model: model.to_string(),
                    ok: false,
                    detail: format!("{}: {}", probe_label(kind), e),
                }
            }
            RunStatus::TimedOut => ModelProbe {
                provider,
                model: model.to_string(),
                ok: false,
                detail: "timed out".to_string(),
            },
        },
        Err(e) => ModelProbe {
            provider,
            model: model.to_string(),
            ok: false,
            detail: format!("transient failure: {e}"),
        },
    }
}

pub fn adapter_for(provider: Provider) -> std::sync::Arc<dyn crate::adapters::Adapter> {
    match provider {
        Provider::Claude => Arc::new(crate::adapters::claude::ClaudeAdapter),
        Provider::Codex => Arc::new(crate::adapters::codex::CodexAdapter),
        Provider::Gemini => Arc::new(crate::adapters::gemini::GeminiAdapter),
    }
}

#[derive(Debug)]
pub struct PreflightReport {
    pub probes: Vec<(ModelCandidate, ModelProbe)>,
}

impl PreflightReport {
    pub fn all_ok(&self) -> bool {
        self.probes.iter().all(|(_, probe)| probe.ok)
    }

    pub fn is_alive(&self, provider: Provider, model: &str) -> bool {
        self.probes.iter().any(|(candidate, probe)| {
            candidate.provider == provider && candidate.model == model && probe.ok
        })
    }

    pub fn dead(&self) -> Vec<&(ModelCandidate, ModelProbe)> {
        self.probes.iter().filter(|(_, probe)| !probe.ok).collect()
    }
}

pub async fn preflight(config: &Config, quota: &QuotaStore) -> PreflightReport {
    let mut probes = Vec::new();
    for candidate in collect_distinct_model_pairs(config) {
        let adapter = adapter_for(candidate.provider);
        let probe = probe_model(adapter, &candidate.model, quota).await;
        probes.push((candidate, probe));
    }
    PreflightReport { probes }
}

pub fn print_preflight(report: &PreflightReport) {
    println!("── session preflight ──");
    for (candidate, probe) in &report.probes {
        if probe.ok {
            println!("  ✓ {}/{}", candidate.provider.as_str(), candidate.model);
        } else {
            println!(
                "  ✗ {}/{} — {}",
                candidate.provider.as_str(),
                candidate.model,
                probe.detail
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::event::Provider;

    fn fake_bin_dir_script(name: &str, script: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    fn fake_bin_dir(name: &str, output: &str) -> tempfile::TempDir {
        fake_bin_dir_script(name, &format!("#!/bin/sh\necho \"{output}\"\n"))
    }

    fn candidate(provider: Provider, model: &str) -> ModelCandidate {
        ModelCandidate {
            provider,
            model: model.to_string(),
        }
    }

    fn probe(provider: Provider, model: &str, ok: bool, detail: &str) -> ModelProbe {
        ModelProbe {
            provider,
            model: model.to_string(),
            ok,
            detail: detail.to_string(),
        }
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

    #[test]
    fn version_falls_back_to_stderr() {
        let dir = fake_bin_dir_script("stderrcli", "#!/bin/sh\necho \"ver 1.0\" >&2\n");
        let status = check_with_path("stderrcli", Some(dir.path().as_os_str()));
        assert!(status.found);
        assert_eq!(status.version.as_deref(), Some("ver 1.0"));
    }

    #[test]
    fn silent_binary_yields_no_version() {
        let dir = fake_bin_dir_script("silentcli", "#!/bin/sh\nexit 0\n");
        let status = check_with_path("silentcli", Some(dir.path().as_os_str()));
        assert!(status.found);
        assert!(status.version.is_none());
    }

    // ── Pure helper unit tests ────────────────────────────────────────────────

    #[test]
    fn probe_label_maps_failure_kind() {
        assert_eq!(probe_label(FailureKind::ModelUnavailable), "unavailable");
        assert_eq!(probe_label(FailureKind::RateLimited), "rate-limited");
        assert_eq!(probe_label(FailureKind::Transient), "transient failure");
    }

    #[test]
    fn collects_distinct_models_across_ladders() {
        // Build a Config where conductor + chairman share the same primary (opus)
        // and conductor has a sonnet fallback — result should deduplicate opus
        // and include sonnet once.
        let config = Config::default();
        let pairs = collect_distinct_model_pairs(&config);

        // There must be at least one entry per distinct model.
        assert!(!pairs.is_empty(), "should have at least one model pair");

        // claude-opus-4-8 appears as conductor primary AND chairman primary —
        // it should appear only once.
        let opus_count = pairs
            .iter()
            .filter(|c| c.model == "claude-opus-4-8" && c.provider == Provider::Claude)
            .count();
        assert_eq!(opus_count, 1, "opus should be deduplicated across roles");

        // claude-sonnet-4-6 is a fallback for both conductor and chairman —
        // it should also appear only once.
        let sonnet_count = pairs
            .iter()
            .filter(|c| c.model == "claude-sonnet-4-6" && c.provider == Provider::Claude)
            .count();
        assert_eq!(sonnet_count, 1, "sonnet fallback should be deduplicated");
    }

    #[test]
    fn collects_distinct_models_with_overlapping_fallbacks() {
        // Construct a minimal Config with explicit overlapping fallbacks.
        let json = r#"{
          "roles": {
            "conductor": {
              "provider": "claude", "model": "opus",
              "fallbacks": [{"provider": "codex", "model": "gpt-x"}]
            },
            "chairman":  {
              "provider": "claude", "model": "opus",
              "fallbacks": [{"provider": "codex", "model": "gpt-x"}]
            },
            "workers":   [{"provider": "codex", "model": "gpt-x"}],
            "reviewer":  {"provider": "codex", "model": "gpt-x"},
            "supervisor": {"provider": "gemini", "model": "gemini-pro"}
          }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        let pairs = collect_distinct_model_pairs(&config);

        // Only 3 distinct (provider,model) pairs: (claude,opus), (codex,gpt-x), (gemini,gemini-pro)
        assert_eq!(
            pairs.len(),
            3,
            "expected 3 distinct pairs, got {}",
            pairs.len()
        );

        let has = |p: Provider, m: &str| pairs.iter().any(|c| c.provider == p && c.model == m);
        assert!(has(Provider::Claude, "opus"));
        assert!(has(Provider::Codex, "gpt-x"));
        assert!(has(Provider::Gemini, "gemini-pro"));
    }

    #[test]
    fn preflight_report_marks_only_conductor_candidate_dead() {
        let report = PreflightReport {
            probes: vec![(
                candidate(Provider::Claude, "claude-opus-4-8"),
                probe(Provider::Claude, "claude-opus-4-8", false, "unavailable"),
            )],
        };

        assert!(!report.is_alive(Provider::Claude, "claude-opus-4-8"));
    }

    #[test]
    fn preflight_report_keeps_conductor_alive_when_other_model_is_dead() {
        let report = PreflightReport {
            probes: vec![
                (
                    candidate(Provider::Claude, "claude-opus-4-8"),
                    probe(Provider::Claude, "claude-opus-4-8", true, "ok"),
                ),
                (
                    candidate(Provider::Codex, "gpt-5.4"),
                    probe(Provider::Codex, "gpt-5.4", false, "rate-limited"),
                ),
            ],
        };

        assert!(report.is_alive(Provider::Claude, "claude-opus-4-8"));
        assert!(!report.all_ok());
    }

    #[test]
    fn preflight_report_is_alive_and_dead_filter_by_exact_pair() {
        let report = PreflightReport {
            probes: vec![
                (
                    candidate(Provider::Claude, "claude-opus-4-8"),
                    probe(Provider::Claude, "claude-opus-4-8", true, "ok"),
                ),
                (
                    candidate(Provider::Codex, "gpt-5.4"),
                    probe(Provider::Codex, "gpt-5.4", false, "rate-limited"),
                ),
                (
                    candidate(Provider::Gemini, "gemini-3-pro-preview"),
                    probe(Provider::Gemini, "gemini-3-pro-preview", true, "ok"),
                ),
            ],
        };

        assert!(report.is_alive(Provider::Claude, "claude-opus-4-8"));
        assert!(!report.is_alive(Provider::Codex, "gpt-5.4"));
        assert!(!report.is_alive(Provider::Claude, "not-probed"));

        let dead = report.dead();
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].0.provider, Provider::Codex);
        assert_eq!(dead[0].0.model, "gpt-5.4");
        assert!(!dead[0].1.ok);
        assert_eq!(dead[0].1.detail, "rate-limited");
    }
}
