//! Conduct contracts: structs, parsers, and orchestration (`run_conduct`).

use crate::adapters::{Adapter, RunRequest};
use crate::orchestrator::changes::capture_changes;
use crate::orchestrator::council::CouncilMember;
use crate::orchestrator::routing::pick_worker_by_provider;
use crate::orchestrator::runner::{run_to_completion, RunStatus};
use crate::quota::QuotaStore;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct Subtask {
    pub id: u32,
    #[serde(default)]
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub depends_note: String,
}

#[derive(Debug, Deserialize)]
pub struct Plan {
    pub subtasks: Vec<Subtask>,
}

pub fn parse_plan(text: &str) -> Option<Plan> {
    super::json_extract::extract_json_object::<Plan>(text)
}

#[derive(Debug, PartialEq)]
pub enum EvalDecision {
    Accept,
    Rework,
    Fail,
}

#[derive(Debug, Deserialize)]
pub struct Evaluation {
    #[serde(deserialize_with = "lenient_decision", default = "default_decision")]
    pub decision: EvalDecision,
    #[serde(default)]
    pub feedback: String,
}

fn default_decision() -> EvalDecision {
    EvalDecision::Rework
}

// Fail-safe: anything unrecognized becomes Rework — never silent acceptance.
fn lenient_decision<'de, D: serde::Deserializer<'de>>(d: D) -> Result<EvalDecision, D::Error> {
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "accept" => EvalDecision::Accept,
        "fail" => EvalDecision::Fail,
        _ => EvalDecision::Rework,
    })
}

pub fn parse_evaluation(text: &str) -> Option<Evaluation> {
    super::json_extract::extract_json_object::<Evaluation>(text)
}

#[derive(Debug, PartialEq)]
pub enum SupervisorStatus {
    Ok,
    Concern,
    Halt,
}

#[derive(Debug, Deserialize)]
pub struct SupervisorVerdict {
    #[serde(deserialize_with = "lenient_status", default = "default_status")]
    pub status: SupervisorStatus,
    #[serde(default)]
    pub note: String,
}

fn default_status() -> SupervisorStatus {
    SupervisorStatus::Concern
}

// Fail-safe: unknown status is a Concern (logged, surfaced), never silent Ok.
fn lenient_status<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SupervisorStatus, D::Error> {
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "ok" => SupervisorStatus::Ok,
        "halt" => SupervisorStatus::Halt,
        _ => SupervisorStatus::Concern,
    })
}

pub fn parse_supervisor(text: &str) -> Option<SupervisorVerdict> {
    super::json_extract::extract_json_object::<SupervisorVerdict>(text)
}

#[derive(Debug, Deserialize)]
pub struct Triage {
    #[serde(default)]
    complexity: String,
}

impl Triage {
    /// Fail-safe: unknown complexity → standard (full pipeline, never skipped).
    pub fn is_trivial(&self) -> bool {
        self.complexity.trim().eq_ignore_ascii_case("trivial")
    }
}

pub fn parse_triage(text: &str) -> Option<Triage> {
    super::json_extract::extract_json_object::<Triage>(text)
}

// ─── Orchestration contracts ─────────────────────────────────────────────────

pub struct RoleHandle {
    pub adapter: Arc<dyn Adapter>,
    pub model: Option<String>,
}

pub struct ConductDeps {
    pub conductor: RoleHandle,
    /// Workers available for routing. Reuses the label/adapter/model triple from
    /// the council module — no new type needed.
    pub workers: Vec<CouncilMember>,
    pub supervisor: Option<RoleHandle>,
}

pub struct ConductOutcome {
    /// Accepted subtask ids, in order.
    pub completed: Vec<u32>,
    /// Set when the supervisor issued a Halt verdict (run aborted).
    pub halted: Option<String>,
    /// Set when the conductor issued Fail or rework attempts were exhausted.
    pub failed: Option<String>,
    pub transcript: serde_json::Value,
}

/// Maximum number of rework rounds per subtask before the whole conduct fails.
pub const MAX_REWORKS: u32 = 2;

/// Conductor/worker loop.
///
/// Flow:
/// 1. Decompose: conductor (advisory) calls `conduct_decompose` → parse plan.
/// 2. For each subtask sequentially:
///    a. Route to least-loaded worker via `pick_worker_by_provider`.
///    b. Worker session (write:true, advisory:false).
///    c. `capture_changes` on cwd.
///    d. Supervisor gate (if configured) — Halt aborts the run.
///    e. Conductor evaluation (advisory) — Accept / Rework / Fail.
///    f. Rework: re-prompt worker (up to MAX_REWORKS); worker Failed/TimedOut counts as a rework attempt.
pub async fn run_conduct(
    task: &str,
    context: &str,
    deps: ConductDeps,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
) -> anyhow::Result<ConductOutcome> {
    use super::prompts;

    // ── extract owned handles so we can move them into async blocks freely ──
    let conductor_adapter = deps.conductor.adapter.clone();
    let conductor_model = deps.conductor.model.clone();
    let supervisor = deps.supervisor;
    let workers = deps.workers;

    // ── Step 1: decompose ────────────────────────────────────────────────────
    let decompose_req = RunRequest {
        prompt: prompts::conduct_decompose(task, context),
        model: conductor_model.clone(),
        cwd: cwd.clone(),
        advisory: true,
        write: false,
    };
    let decompose_out =
        run_to_completion(conductor_adapter.clone(), decompose_req, quota, timeout).await?;
    let plan = parse_plan(&decompose_out.final_text)
        .filter(|p| !p.subtasks.is_empty())
        .ok_or_else(|| anyhow::anyhow!("conductor produced no plan"))?;

    // Build transcript scaffold.
    let plan_summary: Vec<serde_json::Value> = plan
        .subtasks
        .iter()
        .map(|s| serde_json::json!({"id": s.id, "title": s.title}))
        .collect();

    let mut completed: Vec<u32> = Vec::new();
    let mut halted: Option<String> = None;
    let mut failed: Option<String> = None;
    let mut subtask_entries: Vec<serde_json::Value> = Vec::new();

    // Worker providers for routing (stable slice across the loop).
    let worker_providers: Vec<crate::event::Provider> =
        workers.iter().map(|w| w.adapter.provider()).collect();

    // ── Step 2: per-subtask loop ─────────────────────────────────────────────
    'subtask: for subtask in &plan.subtasks {
        let mut attempts: Vec<serde_json::Value> = Vec::new();
        let mut supervisor_entries: Vec<serde_json::Value> = Vec::new();

        // Current prompt starts as the subtask prompt; rework replaces it.
        let original_prompt = subtask.prompt.clone();
        let mut current_prompt = original_prompt.clone();
        let mut previous_changes = String::new();

        for attempt_num in 0..=(MAX_REWORKS as usize) {
            // ── 2a: route ────────────────────────────────────────────────────
            let worker_idx = pick_worker_by_provider(&worker_providers, quota)?;
            let worker = &workers[worker_idx];

            // ── 2b: worker session ──────────────────────────────────────────
            let worker_req = RunRequest {
                prompt: current_prompt.clone(),
                model: worker.model.clone(),
                cwd: cwd.clone(),
                advisory: false,
                write: true,
            };
            let worker_out =
                run_to_completion(worker.adapter.clone(), worker_req, quota, timeout).await?;

            // Worker failure counts as a rework attempt.
            let (worker_text, worker_failed_msg) = match &worker_out.status {
                RunStatus::Completed => (worker_out.final_text.clone(), None),
                RunStatus::Failed(e) => (String::new(), Some(e.clone())),
                RunStatus::TimedOut => (String::new(), Some("worker timed out".to_string())),
            };

            if let Some(err_msg) = worker_failed_msg {
                // Record this as a rework-attempt entry. `worker` lives on the
                // attempt (not the subtask, as the plan sketched) because
                // routing is per-attempt — each retry may land on a different
                // worker.
                attempts.push(serde_json::json!({
                    "attempt": attempt_num,
                    "worker": worker.label,
                    "decision": "rework",
                    "feedback": err_msg,
                    "changes_chars": 0,
                }));
                if attempt_num >= MAX_REWORKS as usize {
                    failed = Some(format!(
                        "subtask {} exhausted reworks (last: {})",
                        subtask.id, err_msg
                    ));
                    subtask_entries.push(serde_json::json!({
                        "id": subtask.id,
                        "title": subtask.title,
                        "attempts": attempts,
                        "supervisor": supervisor_entries,
                    }));
                    break 'subtask;
                }
                // Prepare rework prompt for next attempt.
                current_prompt =
                    prompts::conduct_rework(&original_prompt, &previous_changes, &err_msg);
                continue;
            }

            // ── 2c: capture changes ─────────────────────────────────────────
            // Capture failure is an infrastructure fault (e.g. cwd is no longer
            // a git repo), not worker-quality feedback — propagate loudly rather
            // than burning the rework budget on guaranteed-futile attempts.
            let changes = capture_changes(&cwd)?;
            previous_changes = changes.clone();

            // ── 2d: supervisor gate ─────────────────────────────────────────
            let supervisor_note: Option<String>;
            if let Some(ref sup) = supervisor {
                let progress = build_progress(&completed, subtask, &changes);
                let sup_req = RunRequest {
                    prompt: prompts::supervisor_gate(task, &progress),
                    model: sup.model.clone(),
                    cwd: cwd.clone(),
                    advisory: true,
                    write: false,
                };
                let sup_out =
                    run_to_completion(sup.adapter.clone(), sup_req, quota, timeout).await?;
                let verdict = parse_supervisor(&sup_out.final_text).unwrap_or(SupervisorVerdict {
                    status: SupervisorStatus::Concern,
                    note: "supervisor output unparseable".to_string(),
                });

                supervisor_entries.push(serde_json::json!({
                    "status": status_str(&verdict.status),
                    "note": verdict.note.clone(),
                }));

                match verdict.status {
                    SupervisorStatus::Halt => {
                        halted = Some(verdict.note.clone());
                        subtask_entries.push(serde_json::json!({
                            "id": subtask.id,
                            "title": subtask.title,
                            "attempts": attempts,
                            "supervisor": supervisor_entries,
                        }));
                        break 'subtask;
                    }
                    SupervisorStatus::Concern => {
                        supervisor_note = Some(verdict.note.clone());
                    }
                    SupervisorStatus::Ok => {
                        supervisor_note = None;
                    }
                }
            } else {
                supervisor_note = None;
            }

            // ── 2e: conductor evaluation ────────────────────────────────────
            let eval_req = RunRequest {
                prompt: prompts::conduct_evaluation(
                    &subtask.prompt,
                    &changes,
                    &worker_text,
                    supervisor_note.as_deref(),
                ),
                model: conductor_model.clone(),
                cwd: cwd.clone(),
                advisory: true,
                write: false,
            };
            let eval_out =
                run_to_completion(conductor_adapter.clone(), eval_req, quota, timeout).await?;
            // A conductor infra failure must bail, not charge the worker's
            // rework budget — only a Completed evaluation gets parsed.
            match &eval_out.status {
                RunStatus::Completed => {}
                RunStatus::Failed(e) => anyhow::bail!("conductor evaluation failed: {e}"),
                RunStatus::TimedOut => anyhow::bail!("conductor evaluation timed out"),
            }
            // Fail-safe: unparseable evaluation → Rework (never silent accept).
            let evaluation = parse_evaluation(&eval_out.final_text).unwrap_or(Evaluation {
                decision: EvalDecision::Rework,
                feedback: "evaluation output unparseable".to_string(),
            });

            let decision_str = match evaluation.decision {
                EvalDecision::Accept => "accept",
                EvalDecision::Rework => "rework",
                EvalDecision::Fail => "fail",
            };

            // `worker` recorded per-attempt (see the worker-failure push above).
            attempts.push(serde_json::json!({
                "attempt": attempt_num,
                "worker": worker.label,
                "decision": decision_str,
                "feedback": evaluation.feedback,
                "changes_chars": changes.len(),
            }));

            match evaluation.decision {
                EvalDecision::Accept => {
                    completed.push(subtask.id);
                    subtask_entries.push(serde_json::json!({
                        "id": subtask.id,
                        "title": subtask.title,
                        "attempts": attempts,
                        "supervisor": supervisor_entries,
                    }));
                    continue 'subtask;
                }
                EvalDecision::Fail => {
                    failed = Some(format!(
                        "subtask {} failed: {}",
                        subtask.id, evaluation.feedback
                    ));
                    subtask_entries.push(serde_json::json!({
                        "id": subtask.id,
                        "title": subtask.title,
                        "attempts": attempts,
                        "supervisor": supervisor_entries,
                    }));
                    break 'subtask;
                }
                EvalDecision::Rework => {
                    if attempt_num >= MAX_REWORKS as usize {
                        // Exhausted — fail the whole run.
                        failed = Some(format!(
                            "subtask {} exhausted {} rework attempts: {}",
                            subtask.id, MAX_REWORKS, evaluation.feedback
                        ));
                        subtask_entries.push(serde_json::json!({
                            "id": subtask.id,
                            "title": subtask.title,
                            "attempts": attempts,
                            "supervisor": supervisor_entries,
                        }));
                        break 'subtask;
                    }
                    // Prepare rework for the next iteration.
                    current_prompt =
                        prompts::conduct_rework(&original_prompt, &changes, &evaluation.feedback);
                }
            }
        }
    }

    let transcript = serde_json::json!({
        "kind": "conduct",
        "task": task,
        "plan": plan_summary,
        "subtasks": subtask_entries,
        "completed": completed,
        "halted": halted,
        "failed": failed,
    });

    Ok(ConductOutcome {
        completed,
        halted,
        failed,
        transcript,
    })
}

fn status_str(s: &SupervisorStatus) -> &'static str {
    match s {
        SupervisorStatus::Ok => "ok",
        SupervisorStatus::Concern => "concern",
        SupervisorStatus::Halt => "halt",
    }
}

fn build_progress(completed: &[u32], current: &Subtask, changes: &str) -> String {
    let changes_preview: String = changes.chars().take(500).collect();
    format!(
        "Completed subtasks: {:?}\nCurrent subtask: {} — {}\nLatest changes (preview):\n{}",
        completed, current.id, current.title, changes_preview
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan() {
        let text = r#"```json
{"subtasks":[{"id":1,"title":"add module","prompt":"Create src/x.rs with ...","depends_note":""}]}
```"#;
        let plan = parse_plan(text).unwrap();
        assert_eq!(plan.subtasks.len(), 1);
        assert_eq!(plan.subtasks[0].id, 1);
    }

    #[test]
    fn malformed_plan_returns_none() {
        assert!(parse_plan("no json at all").is_none());
        assert!(parse_plan(r#"{"subtasks": broken"#).is_none());
    }

    #[test]
    fn parses_evaluation_variants() {
        for (s, expected) in [
            (
                r#"{"decision":"accept","feedback":""}"#,
                EvalDecision::Accept,
            ),
            (
                r#"{"decision":"rework","feedback":"missing tests"}"#,
                EvalDecision::Rework,
            ),
            (
                r#"{"decision":"fail","feedback":"impossible"}"#,
                EvalDecision::Fail,
            ),
        ] {
            assert_eq!(parse_evaluation(s).unwrap().decision, expected);
        }
    }

    #[test]
    fn unknown_decision_maps_to_rework() {
        // Fail-safe: an unrecognized decision must not auto-accept.
        let v = parse_evaluation(r#"{"decision":"lgtm!","feedback":"x"}"#).unwrap();
        assert_eq!(v.decision, EvalDecision::Rework);
    }

    #[test]
    fn parses_supervisor_verdict() {
        let v = parse_supervisor(r#"{"status":"halt","note":"scope creep"}"#).unwrap();
        assert_eq!(v.status, SupervisorStatus::Halt);
    }

    #[test]
    fn unknown_supervisor_status_maps_to_concern() {
        let v = parse_supervisor(r#"{"status":"hmm","note":""}"#).unwrap();
        assert_eq!(v.status, SupervisorStatus::Concern);
    }

    #[test]
    fn parses_triage() {
        assert!(parse_triage(r#"{"complexity":"trivial"}"#)
            .unwrap()
            .is_trivial());
        assert!(!parse_triage(r#"{"complexity":"standard"}"#)
            .unwrap()
            .is_trivial());
        assert!(!parse_triage(r#"{"complexity":"weird"}"#)
            .unwrap()
            .is_trivial()); // fail-safe: unknown → standard
    }

    #[test]
    fn decompose_template_example_parses_as_plan() {
        let p = crate::orchestrator::prompts::conduct_decompose("t", "ctx");
        assert!(parse_plan(&p).is_some());
    }

    #[test]
    fn evaluation_template_example_parses() {
        let p = crate::orchestrator::prompts::conduct_evaluation("t", "diff", "report", None);
        assert!(parse_evaluation(&p).is_some());
    }

    #[test]
    fn supervisor_template_example_parses() {
        let p = crate::orchestrator::prompts::supervisor_gate("task", "progress");
        assert!(parse_supervisor(&p).is_some());
    }

    #[test]
    fn triage_template_example_parses() {
        let p = crate::orchestrator::prompts::auto_triage("task");
        assert!(parse_triage(&p).is_some());
    }
}
