//! Auto pipeline: triage → (optional council) → conduct → (optional check command).

use crate::adapters::RunRequest;
use crate::orchestrator::conduct::{run_conduct, ConductDeps, ConductOutcome, RoleHandle};
use crate::orchestrator::council::{run_council, CouncilMember};
use crate::orchestrator::resilience::{run_with_failover, ModelHealth, RetryConfig};
use crate::quota::QuotaStore;
use std::path::PathBuf;
use std::time::Duration;

pub struct AutoDeps {
    pub conduct: ConductDeps,
    pub council_members: Vec<CouncilMember>,
    /// Chairman for council synthesis — a ladder-bearing RoleHandle.
    pub chairman: RoleHandle,
}

pub struct AutoOutcome {
    pub triage_trivial: bool,
    pub council_synthesis: Option<String>,
    pub conduct: ConductOutcome,
    /// (passed, output tail) — only set when check_command was given.
    pub check: Option<(bool, String)>,
    pub transcript: serde_json::Value,
}

/// Full auto pipeline:
///
/// 1. Triage the task via the conductor's ladder (advisory, fail-safe → standard).
/// 2. Trivial → `run_conduct(task, "", ...)` directly.
///    Standard → `run_council` for planning synthesis → `run_conduct(task, &synthesis, ...)`.
/// 3. If fully completed (no halted, no failed) and `check_command` is Some:
///    run `sh -c <cmd>` in cwd (async, capped at `verify.timeoutSecs`, default
///    600s — a hanging check is SIGKILLed and reported as a failed check),
///    capture exit code + last ~2 KiB of output.
/// 4. Build a composed transcript.
///
/// ONE `ModelHealth` is created here and threaded into both `run_council` and
/// `run_conduct`, so a model that dies during planning is skipped during execution.
pub async fn run_auto(
    task: &str,
    deps: AutoDeps,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
    check_command: Option<&str>,
) -> anyhow::Result<AutoOutcome> {
    use super::conduct::parse_triage;
    use super::prompts;

    // ONE ModelHealth for the entire auto run (planning + execution share it).
    let health = ModelHealth::with_retry(RetryConfig::prod());

    // Per-command cap for the check command — same knob as verify commands
    // (verify.timeoutSecs, default 600s). Captured before deps.conduct is
    // moved into run_conduct.
    let check_timeout = crate::orchestrator::verify::command_timeout(deps.conduct.verify.as_ref());

    // ── 1. Triage ────────────────────────────────────────────────────────────
    // Use the conductor's ladder (the same one conduct will use for decompose).
    // Borrow the conductor ladder reference before deps.conduct is moved.
    let triage_fo = {
        let prompt = prompts::auto_triage(task);
        let cwd2 = cwd.clone();
        run_with_failover(
            &deps.conduct.conductor.ladder,
            "triage",
            move |model| RunRequest {
                prompt: prompt.clone(),
                model,
                cwd: cwd2.clone(),
                advisory: true,
                write: false,
            },
            quota,
            &health,
            timeout,
        )
        .await?
    };
    // Triage fallbacks are composed into the auto transcript below.
    let triage_fallbacks = triage_fo.fallbacks;

    // Fail-safe: if triage output doesn't parse, default to standard (full pipeline).
    let trivial = parse_triage(&triage_fo.outcome.final_text)
        .map(|t| t.is_trivial())
        .unwrap_or(false);

    // chairman ladder comes from deps.chairman.ladder (RoleHandle → ladder).
    let chairman_ladder = deps.chairman.ladder;

    // ── 2. Council (standard only) → conduct ─────────────────────────────────
    let (context, council_synthesis, council_transcript) = if trivial {
        (String::new(), None, serde_json::Value::Null)
    } else {
        let question = format!(
            "How should we approach this coding task? Outline the plan, key files, and risks.\n\nTask: {task}"
        );
        let council_outcome = run_council(
            &question,
            deps.council_members,
            chairman_ladder,
            quota,
            cwd.clone(),
            timeout,
            &health,
        )
        .await?;
        let synthesis = council_outcome.synthesis.clone();
        let council_tx = council_outcome.transcript.clone();
        (synthesis.clone(), Some(synthesis), council_tx)
    };

    let conduct_outcome = run_conduct(
        task,
        &context,
        deps.conduct,
        quota,
        cwd.clone(),
        timeout,
        &health,
    )
    .await?;

    // ── 3. Optional check command ─────────────────────────────────────────────
    let check = if conduct_outcome.halted.is_none()
        && conduct_outcome.failed.is_none()
        && !conduct_outcome.completed.is_empty()
    {
        if let Some(cmd) = check_command {
            let out = tokio::time::timeout(
                check_timeout,
                tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(&cwd)
                    .kill_on_drop(true)
                    .output(),
            )
            .await;
            // On Err(Elapsed) the output() future — and with it the
            // kill_on_drop child — is dropped at the end of the statement
            // above, so SIGKILL is issued before we move on.
            match out {
                Err(_elapsed) => Some((
                    false,
                    format!(
                        "TIMEOUT: check command exceeded {}s and was killed: {cmd}",
                        check_timeout.as_secs()
                    ),
                )),
                Ok(output) => {
                    let output = output?;
                    let passed = output.status.success();
                    // Combine stdout + stderr and take last ~2 KiB (char-boundary-safe).
                    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
                    combined.push_str(&String::from_utf8_lossy(&output.stderr));
                    let tail = truncate_tail(&combined, 2048);
                    Some((passed, tail))
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // ── 4. Transcript ─────────────────────────────────────────────────────────
    // Compose run-wide fallbacks: triage + council (in council transcript) +
    // conduct. The council's fallbacks are already in council_transcript["fallbacks"].
    // For the auto-level "fallbacks" array we surface triage + conduct fallbacks
    // (council fallbacks are accessible nested in council_transcript).
    let conduct_fallbacks = conduct_outcome
        .transcript
        .get("fallbacks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut auto_fallbacks: Vec<serde_json::Value> = triage_fallbacks
        .iter()
        .map(|fb| serde_json::json!({"from": fb.from, "to": fb.to, "reason": fb.reason}))
        .collect();
    auto_fallbacks.extend(conduct_fallbacks);

    let transcript = serde_json::json!({
        "kind": "auto",
        "task": task,
        "triage_trivial": trivial,
        "council_synthesis": council_synthesis,
        "council": council_transcript,
        "conduct": conduct_outcome.transcript,
        "fallbacks": auto_fallbacks,
        "check": check.as_ref().map(|(passed, output)| serde_json::json!({
            "passed": passed,
            "output": output,
        })),
    });

    Ok(AutoOutcome {
        triage_trivial: trivial,
        council_synthesis,
        conduct: conduct_outcome,
        check,
        transcript,
    })
}

/// Take the last `max_chars` characters from `s`, aligned to a char boundary.
/// Prepends "[...truncated]\n" when the string was actually trimmed.
fn truncate_tail(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    // Skip the first (char_count - max_chars) characters.
    let skip = char_count - max_chars;
    let tail: String = s.chars().skip(skip).collect();
    format!("[...truncated]\n{tail}")
}
