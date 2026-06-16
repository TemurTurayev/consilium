//! Auto pipeline: triage → (optional council) → conduct → (optional check command).

use crate::adapters::RunRequest;
use crate::orchestrator::conduct::{run_conduct, ConductDeps, ConductOutcome, RoleHandle};
use crate::orchestrator::council::{run_council, CouncilMember};
use crate::orchestrator::runner::run_to_completion;
use crate::quota::QuotaStore;
use std::path::PathBuf;
use std::time::Duration;

pub struct AutoDeps {
    pub conduct: ConductDeps,
    pub council_members: Vec<CouncilMember>,
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
/// 1. Triage the task via the conductor adapter (advisory, advisory parse fail-safe → standard).
/// 2. Trivial → `run_conduct(task, "", ...)` directly.
///    Standard → `run_council` for planning synthesis → `run_conduct(task, &synthesis, ...)`.
/// 3. If fully completed (no halted, no failed) and `check_command` is Some:
///    run `sh -c <cmd>` in cwd (std::process), capture exit code + last ~2 KiB of output.
/// 4. Build a composed transcript.
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

    // ── 1. Triage ────────────────────────────────────────────────────────────
    // Clone the conductor adapter+model out of deps.conduct BEFORE conduct
    // consumes deps.conduct. (An Arc clone is O(1) and borrow-checker-safe.)
    let triage_adapter = deps.conduct.conductor.adapter.clone();
    let triage_model = deps.conduct.conductor.model.clone();

    let triage_req = RunRequest {
        prompt: prompts::auto_triage(task),
        model: triage_model,
        cwd: cwd.clone(),
        advisory: true,
        write: false,
    };
    let triage_out = run_to_completion(triage_adapter, triage_req, quota, timeout).await?;

    // Fail-safe: if triage output doesn't parse, default to standard (full pipeline).
    let trivial = parse_triage(&triage_out.final_text)
        .map(|t| t.is_trivial())
        .unwrap_or(false);

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
            deps.chairman.adapter,
            deps.chairman.model,
            quota,
            cwd.clone(),
            timeout,
        )
        .await?;
        let synthesis = council_outcome.synthesis.clone();
        let council_tx = council_outcome.transcript.clone();
        (synthesis.clone(), Some(synthesis), council_tx)
    };

    let conduct_outcome =
        run_conduct(task, &context, deps.conduct, quota, cwd.clone(), timeout).await?;

    // ── 3. Optional check command ─────────────────────────────────────────────
    let check = if conduct_outcome.halted.is_none()
        && conduct_outcome.failed.is_none()
        && !conduct_outcome.completed.is_empty()
    {
        if let Some(cmd) = check_command {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&cwd)
                .output()?;

            let passed = output.status.success();
            // Combine stdout + stderr and take last ~2 KiB (char-boundary-safe).
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            let tail = truncate_tail(&combined, 2048);
            Some((passed, tail))
        } else {
            None
        }
    } else {
        None
    };

    // ── 4. Transcript ─────────────────────────────────────────────────────────
    let transcript = serde_json::json!({
        "kind": "auto",
        "task": task,
        "triage_trivial": trivial,
        "council_synthesis": council_synthesis,
        "council": council_transcript,
        "conduct": conduct_outcome.transcript,
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
