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

// NOTE: intentionally NOT "claude", "codex", "agy", "grok" — `run_doctor`
// drives the base `consilium doctor` prerequisite GATE (exits 1 if any listed
// binary is missing). Grok is a beta, opt-in fourth provider: it must not
// become a hard requirement for users who never configured it. Its liveness
// is still fully probeable via the generic per-provider path this module and
// `crate::auth` expose (`adapter_for`, `probe_model`, `auth::probe_auth`),
// which `consilium doctor --models`, `consilium auth`, and `consilium models`
// all drive off the user's actual config/catalog rather than this fixed list.
pub fn run_doctor() -> Vec<CliStatus> {
    ["claude", "codex", "agy"]
        .iter()
        .map(|b| check(b))
        .collect()
}

// ── Model probing ──────────────────────────────────────────────────────────────

use crate::adapters::{Adapter, FailureKind, RunRequest, CLI_DEFAULT_MODEL};
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
    /// Why the probe failed; `None` when it answered. A timeout or a launch
    /// error counts as `Transient`.
    pub kind: Option<FailureKind>,
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
    // Claude Code's headless `-p` cold start can take well over 30s (it loads
    // settings, MCP servers, etc.); codex/agy answer faster. Use a generous probe
    // timeout so a slow-but-alive conductor isn't falsely reported dead (which
    // would wrongly abort the conduct/auto preflight gate). A genuinely dead model
    // costs this once each — rare, and `--no-preflight` skips the gate entirely.
    let timeout = Duration::from_secs(120);
    let (kind, detail) = match run_to_completion(adapter.clone(), req, quota, timeout).await {
        Ok(outcome) => match outcome.status {
            RunStatus::Completed => (None, "ok".to_string()),
            RunStatus::Failed(ref e) => {
                let kind = adapter.classify_failure(e);
                (Some(kind), format!("{}: {}", probe_label(kind), e))
            }
            RunStatus::TimedOut => (Some(FailureKind::Transient), "timed out".to_string()),
        },
        Err(e) => (
            Some(FailureKind::Transient),
            format!("transient failure: {e}"),
        ),
    };
    ModelProbe {
        provider,
        model: model.to_string(),
        ok: kind.is_none(),
        kind,
        detail,
    }
}

pub fn adapter_for(provider: Provider) -> std::sync::Arc<dyn crate::adapters::Adapter> {
    match provider {
        Provider::Claude => Arc::new(crate::adapters::claude::ClaudeAdapter),
        Provider::Codex => Arc::new(crate::adapters::codex::CodexAdapter),
        Provider::Gemini => Arc::new(crate::adapters::gemini::GeminiAdapter),
        Provider::Grok => Arc::new(crate::adapters::grok::GrokAdapter),
    }
}

#[derive(Debug)]
pub struct ModelProbeReport {
    pub probes: Vec<(ModelCandidate, ModelProbe)>,
}

impl ModelProbeReport {
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

#[deprecated(note = "use ModelProbeReport; this alias will be removed after v0.3")]
pub type PreflightReport = ModelProbeReport;

/// Splits candidates into (named models, CLI-default rungs), order preserved.
pub(crate) fn split_cli_defaults(
    candidates: Vec<ModelCandidate>,
) -> (Vec<ModelCandidate>, Vec<ModelCandidate>) {
    candidates
        .into_iter()
        .partition(|c| c.model != CLI_DEFAULT_MODEL)
}

/// Every role's full ladder (primary, then fallbacks), one entry per role.
pub(crate) fn role_ladders(config: &Config) -> Vec<Vec<ModelCandidate>> {
    let r = &config.roles;
    [&r.conductor, &r.chairman, &r.reviewer, &r.supervisor]
        .into_iter()
        .chain(r.workers.iter())
        .map(|role| role.ladder())
        .collect()
}

/// Whether a CLI-default rung is worth a probe: only when some role would
/// actually land on it, i.e. its ladder contains the rung and no rung before
/// it answered. It is a last resort, so probing it on every healthy run would
/// spend a call per provider (and ~40s for `agy`) for nothing. Judged per
/// ladder, not per provider: a role whose own named models are dead still
/// needs its default probed even if another role's model answered.
pub(crate) fn needs_cli_default_probe(
    default_rung: &ModelCandidate,
    ladders: &[Vec<ModelCandidate>],
    probes: &[(ModelCandidate, ModelProbe)],
) -> bool {
    let answered = |rung: &ModelCandidate| {
        probes
            .iter()
            .any(|(candidate, probe)| candidate == rung && probe.ok)
    };
    ladders.iter().any(|ladder| {
        ladder
            .iter()
            .position(|rung| rung == default_rung)
            .is_some_and(|at| !ladder[..at].iter().any(answered))
    })
}

async fn probe_candidate(
    candidate: ModelCandidate,
    quota: &QuotaStore,
) -> (ModelCandidate, ModelProbe) {
    let adapter = adapter_for(candidate.provider);
    let probe = probe_model(adapter, &candidate.model, quota).await;
    (candidate, probe)
}

/// Probes every distinct configured model: the named ones first, then a
/// provider's CLI-default rung only if it is needed (see
/// `needs_cli_default_probe`).
pub async fn preflight(config: &Config, quota: &QuotaStore) -> ModelProbeReport {
    let (named, defaults) = split_cli_defaults(collect_distinct_model_pairs(config));
    let ladders = role_ladders(config);
    let mut probes = Vec::new();
    for candidate in named {
        probes.push(probe_candidate(candidate, quota).await);
    }
    for candidate in defaults {
        if needs_cli_default_probe(&candidate, &ladders, &probes) {
            probes.push(probe_candidate(candidate, quota).await);
        }
    }
    ModelProbeReport { probes }
}

/// A short, actionable remediation hint for a failed probe, matched on the error
/// detail. Display-only: a missed match just yields the generic hint, so there's
/// no correctness risk in the (necessarily fuzzy) string matching.
pub(crate) fn remediation_hint(detail: &str) -> &'static str {
    let d = detail.to_ascii_lowercase();
    if d.starts_with(probe_label(FailureKind::ModelUnavailable)) {
        "this model is not available to your account (retired, or not in your plan) — run `consilium models --write` to switch to one that is, or remove it from consilium.config.json"
    } else if d.contains("outdated") || d.contains("upgrade required") {
        "this CLI version is too old for the provider — update the CLI (its message names the command), then re-run"
    } else if d.contains("no longer supported") || d.contains("ineligible") || d.contains("migrate")
    {
        "provider tier looks deprecated — migrate this CLI's auth (a supported tier or API key)"
    } else if d.contains("401")
        || d.contains("authenticat")
        || d.contains("unauthor")
        || d.contains("credential")
    {
        "re-authenticate this CLI — Claude: run `claude setup-token` then export CLAUDE_CODE_OAUTH_TOKEN; others: re-login — then re-run"
    } else if d.contains("rate") && d.contains("limit") {
        "rate-limited — wait and retry, or reduce concurrency"
    } else if d.contains("no output") || d.contains("produced no") {
        "CLI returned nothing — run it directly (e.g. `agy -p hi`) to see its error; usually an expired/unsupported session"
    } else {
        "re-authenticate or update this CLI, then re-run `consilium doctor --models`"
    }
}

/// How much of a CLI's error text a status line shows.
pub(crate) const DETAIL_DISPLAY_CHARS: usize = 320;

/// A CLI error squeezed onto one line for a status listing: runs of whitespace
/// (including newlines) become single spaces, and anything beyond `max_chars`
/// characters is cut with an ellipsis. Provider CLIs answer with multi-line
/// text — a pretty-printed JSON blob, or a reason followed by a long model
/// list — which otherwise buries the status lines around it.
pub(crate) fn one_line(detail: &str, max_chars: usize) -> String {
    let flat = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let kept: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

pub fn print_preflight(report: &ModelProbeReport) {
    println!("── session preflight ──");
    for (candidate, probe) in &report.probes {
        if probe.ok {
            println!("  ✓ {}/{}", candidate.provider.as_str(), candidate.model);
        } else {
            println!(
                "  ✗ {}/{} — {}",
                candidate.provider.as_str(),
                candidate.model,
                one_line(&probe.detail, DETAIL_DISPLAY_CHARS)
            );
            println!("      → {}", remediation_hint(&probe.detail));
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
            kind: (!ok).then_some(FailureKind::Transient),
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
    fn remediation_hint_matches_known_failures() {
        assert!(
            remediation_hint("API Error: 401 authentication_error: Invalid credentials")
                .contains("re-authenticate")
        );
        assert!(remediation_hint("gemini produced no output").contains("run it directly"));
        assert!(
            remediation_hint("This client is no longer supported; migrate to Antigravity")
                .contains("migrate")
        );
        assert!(remediation_hint("error: rate limit exceeded").contains("rate-limited"));
        // Real grok 0.2.87 refusal (HTTP 426) once xAI required a newer client.
        assert!(remediation_hint(
            "API error (status 426 Upgrade Required): Your Grok CLI version (0.2.87) is outdated. Please update to version 1.0.13 or later via `grok update`"
        )
        .contains("update the CLI"));
        // The "returned nothing" hint must name a CLI that still exists.
        assert!(!remediation_hint("agy (antigravity) produced no output").contains("gemini -p"));
        assert!(!remediation_hint("some unrecognized failure").is_empty());
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

        // claude-opus-5-5 appears as conductor primary AND chairman primary —
        // it should appear only once.
        let count = |model: &str| {
            pairs
                .iter()
                .filter(|c| c.model == model && c.provider == Provider::Claude)
                .count()
        };
        assert_eq!(
            count("claude-opus-5-5"),
            1,
            "opus should be deduplicated across roles"
        );

        // claude-sonnet-5-5 is a fallback for both conductor and chairman —
        // it should also appear only once.
        assert_eq!(
            count("claude-sonnet-5-5"),
            1,
            "sonnet fallback should be deduplicated"
        );
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
    fn one_line_collapses_a_multi_line_cli_error_and_caps_its_length() {
        // Shape of the real agy 1.5 error: the reason, then a long model list.
        let detail = "unavailable: error: invalid model selection\nAvailable models:\n  Gemini 4 Argon (High)\n  Gemini 4 Argon (Low)";
        assert_eq!(
            one_line(detail, 200),
            "unavailable: error: invalid model selection Available models: Gemini 4 Argon (High) Gemini 4 Argon (Low)"
        );
        // capped to at most `max_chars` characters (never mid-character),
        // ending in an ellipsis
        assert_eq!(one_line(detail, 20), "unavailable: error:…");
        assert_eq!(one_line("привет\nмир", 8), "привет…");
        assert_eq!(one_line("привет\nмир", 9), "привет м…");
        // short single-line details pass through untouched
        assert_eq!(one_line("ok", 200), "ok");
    }

    #[test]
    fn remediation_hint_points_an_unavailable_model_at_consilium_models() {
        // Real codex-cli 0.160.1 refusal for a plan-gated model.
        let detail = "unavailable: {\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'gpt-6.1-sol' model is not supported when using Codex with a ChatGPT account.\",\"code\":\"model_not_found\"}}";
        assert!(remediation_hint(detail).contains("consilium models --write"));
    }

    #[test]
    fn default_config_preflight_probes_named_models_before_any_cli_default() {
        let (named, defaults) =
            split_cli_defaults(collect_distinct_model_pairs(&Config::default()));
        assert!(named.iter().all(|c| c.model != CLI_DEFAULT_MODEL));
        // one named model per provider tier in the default lineup — no more
        // probes than v0.2.0 made
        let named_models: Vec<&str> = named.iter().map(|c| c.model.as_str()).collect();
        assert_eq!(
            named_models,
            [
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "gpt-6.1-sol",
                "gemini-4-argon-high"
            ]
        );
        // one CLI-default rung per provider, deduplicated across roles
        let default_providers: Vec<Provider> = defaults.iter().map(|c| c.provider).collect();
        assert_eq!(
            default_providers,
            [Provider::Claude, Provider::Codex, Provider::Gemini]
        );
    }

    fn ladder(rungs: &[(Provider, &str)]) -> Vec<ModelCandidate> {
        rungs.iter().map(|(p, m)| candidate(*p, m)).collect()
    }

    #[test]
    fn cli_default_is_probed_only_when_a_role_would_actually_land_on_it() {
        let probes = vec![
            (
                candidate(Provider::Claude, "claude-opus-5-5"),
                probe(Provider::Claude, "claude-opus-5-5", true, "ok"),
            ),
            (
                candidate(Provider::Codex, "gpt-6.1-sol"),
                probe(Provider::Codex, "gpt-6.1-sol", false, "unavailable"),
            ),
        ];
        let claude_default = candidate(Provider::Claude, CLI_DEFAULT_MODEL);
        let codex_default = candidate(Provider::Codex, CLI_DEFAULT_MODEL);
        let gemini_default = candidate(Provider::Gemini, CLI_DEFAULT_MODEL);
        let ladders = vec![
            ladder(&[
                (Provider::Claude, "claude-opus-5-5"),
                (Provider::Claude, CLI_DEFAULT_MODEL),
            ]),
            ladder(&[
                (Provider::Codex, "gpt-6.1-sol"),
                (Provider::Codex, CLI_DEFAULT_MODEL),
            ]),
            ladder(&[(Provider::Gemini, CLI_DEFAULT_MODEL)]),
        ];
        // an earlier rung answered → the last-resort rung is not worth a probe
        assert!(!needs_cli_default_probe(&claude_default, &ladders, &probes));
        // the only named rung is dead → the CLI default is what will run
        assert!(needs_cli_default_probe(&codex_default, &ladders, &probes));
        // a `default`-only role always lands on it
        assert!(needs_cli_default_probe(&gemini_default, &ladders, &probes));
    }

    /// The conductor gate looks at the conductor's own ladder. If its only
    /// named model is dead, its CLI-default rung must be probed even though
    /// another role's Claude model answered — otherwise the gate would stop a
    /// run that the default rung can carry.
    #[test]
    fn cli_default_is_judged_per_ladder_not_per_provider() {
        let probes = vec![
            (
                candidate(Provider::Claude, "claude-opus-5-5"),
                probe(Provider::Claude, "claude-opus-5-5", false, "unavailable"),
            ),
            (
                candidate(Provider::Claude, "claude-sonnet-5-5"),
                probe(Provider::Claude, "claude-sonnet-5-5", true, "ok"),
            ),
        ];
        let ladders = vec![
            // conductor: opus is dead, so it lands on the CLI default
            ladder(&[
                (Provider::Claude, "claude-opus-5-5"),
                (Provider::Claude, CLI_DEFAULT_MODEL),
            ]),
            // a worker pinned to sonnet, which answers
            ladder(&[(Provider::Claude, "claude-sonnet-5-5")]),
        ];
        assert!(needs_cli_default_probe(
            &candidate(Provider::Claude, CLI_DEFAULT_MODEL),
            &ladders,
            &probes
        ));
    }

    #[test]
    fn preflight_report_marks_only_conductor_candidate_dead() {
        let report = ModelProbeReport {
            probes: vec![(
                candidate(Provider::Claude, "claude-opus-4-8"),
                probe(Provider::Claude, "claude-opus-4-8", false, "unavailable"),
            )],
        };

        assert!(!report.is_alive(Provider::Claude, "claude-opus-4-8"));
    }

    #[test]
    fn preflight_report_keeps_conductor_alive_when_other_model_is_dead() {
        let report = ModelProbeReport {
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
        let report = ModelProbeReport {
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
