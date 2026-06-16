//! Conduct contracts: structs, parsers, and orchestration (`run_conduct`).

use crate::adapters::RunRequest;
use crate::config::VerifyConfig;
use crate::orchestrator::changes::capture_changes;
use crate::orchestrator::council::CouncilMember;
use crate::orchestrator::resilience::{run_with_failover, Fallback, ModelHealth, Rung};
use crate::orchestrator::routing::pick_worker_by_provider;
use crate::orchestrator::verify;
use crate::quota::QuotaStore;
use serde::Deserialize;
use std::path::PathBuf;
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

// ─── Arbiter verdict ─────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub enum ArbiterDecision {
    Ship,
    Fail,
}

#[derive(Debug, Deserialize)]
pub struct ArbiterVerdict {
    #[serde(
        deserialize_with = "lenient_arbiter_decision",
        default = "default_arbiter_decision"
    )]
    pub decision: ArbiterDecision,
    #[serde(default)]
    pub reason: String,
}

fn default_arbiter_decision() -> ArbiterDecision {
    ArbiterDecision::Fail
}

// Fail-safe: anything not explicitly "ship" → Fail (never silent ship).
fn lenient_arbiter_decision<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<ArbiterDecision, D::Error> {
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "ship" => ArbiterDecision::Ship,
        _ => ArbiterDecision::Fail,
    })
}

pub fn parse_arbiter(text: &str) -> Option<ArbiterVerdict> {
    super::json_extract::extract_json_object::<ArbiterVerdict>(text)
}

// ─── Orchestration contracts ─────────────────────────────────────────────────

/// A role's failover ladder: one or more (candidate, adapter) rungs, primary
/// first. Every model call goes through `run_with_failover` over this ladder.
pub struct RoleHandle {
    pub ladder: Vec<Rung>,
}

pub struct ConductDeps {
    pub conductor: RoleHandle,
    /// Workers available for routing. Reuses the label/ladder triple from the
    /// council module — no new type needed.
    pub workers: Vec<CouncilMember>,
    pub supervisor: Option<RoleHandle>,
    pub reviewer: Option<RoleHandle>,
    pub arbiter: Option<RoleHandle>,
    /// Optional build/test/lint verifier. When Some, `run_verify` is called
    /// after each worker attempt and a failed build/test overrides Accept→Rework.
    pub verify: Option<VerifyConfig>,
}

#[derive(Debug)]
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
/// 2. For each subtask sequentially: route to least-loaded worker
///    (`pick_worker_by_provider`); run the worker (write:true, advisory:false)
///    via run_with_failover; `capture_changes` on cwd; supervisor gate (Halt
///    aborts the run); conductor evaluation (Accept / Rework / Fail); rework
///    re-prompts the worker up to MAX_REWORKS (all-rungs-fail counts as an attempt).
///
/// All model calls go through `run_with_failover` with the shared `health`
/// registry, so a model that dies during planning is skipped during execution.
pub async fn run_conduct(
    task: &str,
    context: &str,
    deps: ConductDeps,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
    health: &ModelHealth,
) -> anyhow::Result<ConductOutcome> {
    use super::prompts;

    // ── extract owned handles ─────────────────────────────────────────────────
    let conductor_ladder = deps.conductor.ladder;
    let supervisor = deps.supervisor;
    let reviewer = deps.reviewer;
    let arbiter = deps.arbiter;
    let workers = deps.workers;
    let verify_cfg = deps.verify;

    // Accumulate all run-wide fallbacks for the transcript.
    let mut all_fallbacks: Vec<Fallback> = Vec::new();

    // ── Step 1: decompose ────────────────────────────────────────────────────
    // run_with_failover bails when ALL rungs fail — that IS the infra-failure
    // case. On Ok, the outcome is always Completed (failover only returns on
    // success). So we propagate the Err directly via `?` and then check for an
    // unparseable (but technically Completed) plan separately.
    let decompose_fo = {
        let prompt = prompts::conduct_decompose(task, context);
        let cwd2 = cwd.clone();
        run_with_failover(
            &conductor_ladder,
            "conductor",
            move |model| RunRequest {
                prompt: prompt.clone(),
                model,
                cwd: cwd2.clone(),
                advisory: true,
                write: false,
            },
            quota,
            health,
            timeout,
        )
        .await?
    };
    all_fallbacks.extend(decompose_fo.fallbacks);
    let plan = parse_plan(&decompose_fo.outcome.final_text)
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

    // Worker providers for routing: use the primary (first) rung's provider.
    let worker_providers: Vec<crate::event::Provider> = workers
        .iter()
        .map(|w| {
            w.ladder
                .first()
                .map(|r| r.candidate.provider)
                .unwrap_or(crate::event::Provider::Claude)
        })
        .collect();

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

            // ── 2b: worker session via failover ─────────────────────────────
            // run_with_failover returns Err only when ALL rungs fail — treat
            // that as a worker-failed-attempt (feedback = the error message).
            let worker_label = worker.label.clone();
            let worker_result = {
                let prompt = current_prompt.clone();
                let cwd2 = cwd.clone();
                run_with_failover(
                    &worker.ladder,
                    &worker_label,
                    move |model| RunRequest {
                        prompt: prompt.clone(),
                        model,
                        cwd: cwd2.clone(),
                        advisory: false,
                        write: true,
                    },
                    quota,
                    health,
                    timeout,
                )
                .await
            };

            let (worker_text, worker_failed_msg) = match worker_result {
                Ok(fo) => {
                    all_fallbacks.extend(fo.fallbacks);
                    (fo.outcome.final_text, None)
                }
                Err(e) => (String::new(), Some(e.to_string())),
            };

            if let Some(err_msg) = worker_failed_msg {
                // All worker rungs failed — counts as a rework attempt.
                // No verify ran because the worker itself failed.
                attempts.push(serde_json::json!({
                    "attempt": attempt_num,
                    "worker": worker.label,
                    "decision": "rework",
                    "feedback": err_msg,
                    "changes_chars": 0,
                    "verify": "not_run",
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
            // Capture failure is an infrastructure fault — propagate loudly.
            let changes = capture_changes(&cwd)?;
            previous_changes = changes.clone();

            // ── 2c2: verify (build/test/lint) ───────────────────────────────
            // Runs after every worker attempt. A failed build/test overrides
            // Accept→Rework (grounding rule); lint failure is advisory only.
            let verify_outcome = verify::run_verify(&cwd, verify_cfg.as_ref()).await;

            // ── 2d: supervisor gate ─────────────────────────────────────────
            let supervisor_note: Option<String>;
            if let Some(ref sup) = supervisor {
                let progress = build_progress(&completed, subtask, &changes);
                let fo = {
                    let prompt = prompts::supervisor_gate(task, &progress);
                    let cwd2 = cwd.clone();
                    run_with_failover(
                        &sup.ladder,
                        "supervisor",
                        move |model| RunRequest {
                            prompt: prompt.clone(),
                            model,
                            cwd: cwd2.clone(),
                            advisory: true,
                            write: false,
                        },
                        quota,
                        health,
                        timeout,
                    )
                    .await?
                };
                all_fallbacks.extend(fo.fallbacks);
                let verdict =
                    parse_supervisor(&fo.outcome.final_text).unwrap_or(SupervisorVerdict {
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
            // run_with_failover Err means all conductor rungs are dead.
            let verify_summary_for_prompt = if verify_outcome.ran {
                verify_outcome.summary.as_str()
            } else {
                "(no verifier ran)"
            };
            let eval_fo = {
                let prompt = prompts::conduct_evaluation(
                    &subtask.prompt,
                    &changes,
                    &worker_text,
                    verify_summary_for_prompt,
                    supervisor_note.as_deref(),
                );
                let cwd2 = cwd.clone();
                run_with_failover(
                    &conductor_ladder,
                    "conductor",
                    move |model| RunRequest {
                        prompt: prompt.clone(),
                        model,
                        cwd: cwd2.clone(),
                        advisory: true,
                        write: false,
                    },
                    quota,
                    health,
                    timeout,
                )
                .await
                .map_err(|e| anyhow::anyhow!("conductor evaluation failed: {e}"))?
            };
            all_fallbacks.extend(eval_fo.fallbacks);

            // Fail-safe: unparseable evaluation → Rework (never silent accept).
            let mut evaluation =
                parse_evaluation(&eval_fo.outcome.final_text).unwrap_or(Evaluation {
                    decision: EvalDecision::Rework,
                    feedback: "evaluation output unparseable".to_string(),
                });

            // ── GROUNDING RULE (keystone) ───────────────────────────────────
            // If verify ran and failed, the subtask CANNOT be accepted this
            // attempt — regardless of the conductor's text opinion. Override
            // Accept→Rework with the failure summary as feedback. Passed or
            // not-run → conductor's decision stands.
            if verify_outcome.ran
                && !verify_outcome.passed
                && evaluation.decision == EvalDecision::Accept
            {
                evaluation = Evaluation {
                    decision: EvalDecision::Rework,
                    feedback: format!(
                        "Build/test failed; fix before acceptance:\n{}",
                        verify_outcome.summary
                    ),
                };
            }

            let verify_status = match (verify_outcome.ran, verify_outcome.passed) {
                (false, _) => "not_run",
                (true, true) => "passed",
                (true, false) => "failed",
            };

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
                "verify": verify_status,
            }));

            match evaluation.decision {
                EvalDecision::Accept => {
                    // ── 2f: review gate (if configured, advisory) ───────────
                    if let Some(ref rev) = reviewer {
                        let (review_result, rev_fallbacks) = super::review::run_review_ladder(
                            &changes,
                            &rev.ladder,
                            health,
                            quota,
                            cwd.clone(),
                            timeout,
                        )
                        .await?;
                        all_fallbacks.extend(rev_fallbacks);

                        // Determine whether review blocks.
                        let review_blocks = match &review_result.verdict {
                            Some(v) => v.has_critical(),
                            None => true, // unparseable → fail-closed
                        };

                        // Summarise findings for feedback / arbiter.
                        let findings_text = match &review_result.verdict {
                            Some(v) => serde_json::to_string(&serde_json::json!({
                                "findings": v.findings.iter().map(|f| serde_json::json!({
                                    "severity": format!("{:?}", f.severity).to_lowercase(),
                                    "file": f.file,
                                    "description": f.description,
                                })).collect::<Vec<_>>()
                            }))
                            .unwrap_or_else(|_| review_result.raw_review.clone()),
                            None => "reviewer output unparseable".to_string(),
                        };

                        let review_status = if review_result.verdict.is_none() {
                            "unparseable"
                        } else if review_blocks {
                            "critical"
                        } else {
                            "clean"
                        };
                        // Patch the last attempt entry with the review outcome.
                        if let Some(last) = attempts.last_mut() {
                            if let Some(obj) = last.as_object_mut() {
                                obj.insert(
                                    "review".to_string(),
                                    serde_json::Value::String(review_status.to_string()),
                                );
                            }
                        }

                        if review_blocks {
                            if attempt_num >= MAX_REWORKS as usize {
                                // Exhausted with review still blocking → try arbiter.
                                if let Some(ref arb) = arbiter {
                                    let arb_fo = {
                                        let prompt = prompts::arbiter_decide(
                                            &subtask.prompt,
                                            &changes,
                                            &findings_text,
                                        );
                                        let cwd2 = cwd.clone();
                                        run_with_failover(
                                            &arb.ladder,
                                            "arbiter",
                                            move |model| RunRequest {
                                                prompt: prompt.clone(),
                                                model,
                                                cwd: cwd2.clone(),
                                                advisory: true,
                                                write: false,
                                            },
                                            quota,
                                            health,
                                            timeout,
                                        )
                                        .await?
                                    };
                                    all_fallbacks.extend(arb_fo.fallbacks);
                                    // Fail-safe: unparseable → Fail.
                                    let arb_verdict = parse_arbiter(&arb_fo.outcome.final_text)
                                        .unwrap_or(ArbiterVerdict {
                                            decision: ArbiterDecision::Fail,
                                            reason: "arbiter output unparseable".to_string(),
                                        });

                                    if arb_verdict.decision == ArbiterDecision::Ship {
                                        completed.push(subtask.id);
                                        subtask_entries.push(serde_json::json!({
                                            "id": subtask.id,
                                            "title": subtask.title,
                                            "attempts": attempts,
                                            "supervisor": supervisor_entries,
                                            "arbiter": {
                                                "decision": "ship",
                                                "reason": arb_verdict.reason,
                                            },
                                        }));
                                        continue 'subtask;
                                    } else {
                                        failed = Some(format!(
                                            "subtask {} arbiter failed: {}",
                                            subtask.id, arb_verdict.reason
                                        ));
                                        subtask_entries.push(serde_json::json!({
                                            "id": subtask.id,
                                            "title": subtask.title,
                                            "attempts": attempts,
                                            "supervisor": supervisor_entries,
                                            "arbiter": {
                                                "decision": "fail",
                                                "reason": arb_verdict.reason,
                                            },
                                        }));
                                        break 'subtask;
                                    }
                                }

                                // No arbiter — just fail.
                                failed = Some(format!(
                                    "subtask {} exhausted {} rework attempts (review gate): {}",
                                    subtask.id, MAX_REWORKS, findings_text
                                ));
                                subtask_entries.push(serde_json::json!({
                                    "id": subtask.id,
                                    "title": subtask.title,
                                    "attempts": attempts,
                                    "supervisor": supervisor_entries,
                                }));
                                break 'subtask;
                            }

                            // Not yet exhausted — rework with review findings.
                            current_prompt =
                                prompts::conduct_rework(&original_prompt, &changes, &findings_text);
                            continue;
                        }
                    }

                    // Review clean (or no reviewer) → accept.
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

    let fallbacks_json: Vec<serde_json::Value> = all_fallbacks
        .iter()
        .map(|fb| {
            serde_json::json!({
                "from": fb.from,
                "to": fb.to,
                "reason": fb.reason,
            })
        })
        .collect();

    let transcript = serde_json::json!({
        "kind": "conduct",
        "task": task,
        "plan": plan_summary,
        "subtasks": subtask_entries,
        "completed": completed,
        "halted": halted,
        "failed": failed,
        "fallbacks": fallbacks_json,
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
        let p = crate::orchestrator::prompts::conduct_evaluation(
            "t",
            "diff",
            "report",
            "(not run)",
            None,
        );
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

    // ── Arbiter verdict unit tests ────────────────────────────────────────────

    #[test]
    fn arbiter_parses_ship() {
        let v = parse_arbiter(r#"{"decision":"ship","reason":"findings are noise"}"#).unwrap();
        assert_eq!(v.decision, ArbiterDecision::Ship);
        assert_eq!(v.reason, "findings are noise");
    }

    #[test]
    fn arbiter_parses_fail() {
        let v = parse_arbiter(r#"{"decision":"fail","reason":"real blocker"}"#).unwrap();
        assert_eq!(v.decision, ArbiterDecision::Fail);
    }

    #[test]
    fn arbiter_unknown_maps_to_fail() {
        // Fail-safe: unrecognized decision must never silently ship.
        let v = parse_arbiter(r#"{"decision":"maybe","reason":"unsure"}"#).unwrap();
        assert_eq!(v.decision, ArbiterDecision::Fail);
    }

    #[test]
    fn arbiter_template_example_parses() {
        let p = crate::orchestrator::prompts::arbiter_decide("subtask", "changes", "findings");
        let v = parse_arbiter(&p).expect("arbiter template example must parse");
        // Template example decision is "ship".
        assert_eq!(v.decision, ArbiterDecision::Ship);
    }
}
