//! Conduct contracts: structs, parsers, and orchestration (`run_conduct`).

use crate::adapters::RunRequest;
use crate::config::{ConductorMemoryConfig, VerifyConfig};
use crate::orchestrator::changes::{capture_changed_files, capture_changes};
use crate::orchestrator::council::CouncilMember;
use crate::orchestrator::resilience::{run_with_failover, Fallback, ModelHealth, Rung};
use crate::orchestrator::routing::pick_worker_by_provider;
use crate::orchestrator::stagnation;
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
    /// Conductor working memory (plan ledger + attempt history). When enabled,
    /// folded prior-subtask status + this subtask's prior attempts are injected
    /// into the conductor-facing prompts. `Default` is enabled.
    pub memory: ConductorMemoryConfig,
    /// Cross-family review (Finding 7): when true, the review + arbiter gates
    /// route a subtask's diff to a reviewer of a DIFFERENT family than the
    /// worker that produced it. `Default`/false reproduces today's behavior.
    pub cross_family_review: bool,
    /// The optional total wall-clock budget for the whole run; `None` means
    /// unlimited (current behavior).
    pub budget: Option<std::time::Duration>,
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
    let mem_on = deps.memory.enabled;
    let ledger_cap = deps.memory.ledger_char_cap;
    let hist_cap = deps.memory.attempt_history_char_cap;
    let cross_family = deps.cross_family_review;
    let budget = deps.budget;
    // Whole-run wall-clock start for the budget governor — captured before
    // decompose so a slow plan also counts against the budget.
    let run_start = std::time::Instant::now();

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

    // Files already dirty at run start — so the worker blackboard reports only
    // what THIS run changed. Best-effort (cosmetic context, never load-bearing).
    let run_start_files = capture_changed_files(&cwd).unwrap_or_default();

    // ── Step 2: per-subtask loop ─────────────────────────────────────────────
    // Subtasks run SEQUENTIALLY in the shared `cwd` and (per the decompose
    // contract) touch DISJOINT files, so a later worker reading the live tree
    // cannot clobber an earlier one. Worktree-per-subtask isolation is deferred
    // until real parallel workers land (it would also break the cross-subtask
    // inheritance the blackboard provides by starting each worker from HEAD).
    'subtask: for subtask in &plan.subtasks {
        if let Some(b) = budget {
            let elapsed = run_start.elapsed();
            if elapsed >= b {
                failed = Some(format!(
                    "budget exceeded: {:.1}s wall-clock elapsed; shipped {} of {} subtasks",
                    elapsed.as_secs_f64(),
                    completed.len(),
                    plan.subtasks.len()
                ));
                break;
            }
        }

        let mut attempts: Vec<serde_json::Value> = Vec::new();
        let mut supervisor_entries: Vec<serde_json::Value> = Vec::new();

        let original_prompt = subtask.prompt.clone();
        let mut previous_changes = String::new();
        // Cross-subtask ledger: folded status of the prior finished subtasks.
        // Stable for this whole subtask — `subtask_entries` only grows at the
        // terminal sites, which end this iteration.
        let ledger_str = mem_ledger(mem_on, &subtask_entries, ledger_cap);
        // Worker blackboard (read-only inheritance, INITIAL prompt only): the
        // mechanical roster of prior finished subtasks + files this run touched.
        let changed_this_run: Vec<String> = capture_changed_files(&cwd)
            .unwrap_or_default()
            .into_iter()
            .filter(|f| !run_start_files.contains(f))
            .collect();
        let blackboard = mem_blackboard(mem_on, &subtask_entries, &changed_this_run, ledger_cap);
        let mut current_prompt = prompts::conduct_initial(&original_prompt, blackboard.as_deref());
        // P1.5 stagnation: fingerprints of prior attempts this subtask, to detect
        // a worker that's spinning (reproducing an earlier diff + verify result).
        let mut fingerprints: Vec<u64> = Vec::new();
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
                    subtask_entries.push(build_subtask_entry(
                        subtask.id,
                        &subtask.title,
                        "failed",
                        &attempts,
                        &supervisor_entries,
                    ));
                    break 'subtask;
                }
                // Prepare rework prompt for next attempt. History includes the
                // just-recorded failed round.
                let history = mem_history(mem_on, &attempts, hist_cap);
                current_prompt = prompts::conduct_rework(
                    &original_prompt,
                    &previous_changes,
                    &err_msg,
                    history.as_deref(),
                );
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

            // P1.5 stagnation: a stall = this attempt reproduced a prior attempt's
            // exact diff + verify result (the worker is spinning). Computed here
            // (post-verify) but acted on only in the Rework arm, where it turns a
            // would-be rework into an early "stalled" stop.
            let stalled = {
                let fp = stagnation::fingerprint(&changes, &verify_outcome.summary);
                let s = stagnation::is_stalled(&fingerprints, fp);
                fingerprints.push(fp);
                s
            };

            // History of PRIOR attempts (the current round isn't recorded until
            // after evaluation), for the supervisor + conductor evaluation.
            let prior_history = mem_history(mem_on, &attempts, hist_cap);

            // ── 2d: supervisor gate ─────────────────────────────────────────
            let supervisor_note: Option<String>;
            if let Some(ref sup) = supervisor {
                let progress = build_progress(task, &plan.subtasks, &completed, subtask, &changes);
                let sup_result = {
                    let prompt = prompts::supervisor_gate(
                        task,
                        &progress,
                        ledger_str.as_deref(),
                        prior_history.as_deref(),
                    );
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
                    .await
                };
                match sup_result {
                    Ok(fo) => {
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
                                subtask_entries.push(build_subtask_entry(
                                    subtask.id,
                                    &subtask.title,
                                    "halted",
                                    &attempts,
                                    &supervisor_entries,
                                ));
                                break 'subtask;
                            }
                            SupervisorStatus::Concern => {
                                supervisor_note = Some(verdict.note.clone());
                            }
                            SupervisorStatus::Ok => {
                                supervisor_note = None;
                            }
                        }
                    }
                    Err(e) => {
                        // Advisory gate: a transient supervisor failure (all rungs
                        // down) must NOT kill the run — the supervisor only raises
                        // Concern/Halt. Degrade to "no verdict this attempt" and
                        // proceed; record it in the transcript so it stays visible.
                        tracing::warn!(error = %e, "supervisor unavailable; proceeding without its verdict");
                        supervisor_entries.push(serde_json::json!({
                            "status": "unavailable",
                            "note": format!("supervisor failed: {e}"),
                        }));
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
                    ledger_str.as_deref(),
                    prior_history.as_deref(),
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
                    // The gate's logic lives in run_review_gate; here we just act
                    // on its decision, keeping this arm flat.
                    let decision = match reviewer {
                        Some(ref rev) => {
                            run_review_gate(
                                &changes,
                                subtask,
                                rev,
                                arbiter.as_ref(),
                                cross_family,
                                &workers,
                                worker_providers[worker_idx],
                                attempt_num,
                                &mut attempts,
                                &mut all_fallbacks,
                                ledger_str.as_deref(),
                                mem_on,
                                hist_cap,
                                quota,
                                health,
                                &cwd,
                                timeout,
                            )
                            .await
                        }
                        None => GateDecision::Accept,
                    };

                    match decision {
                        GateDecision::Accept => {
                            completed.push(subtask.id);
                            subtask_entries.push(build_subtask_entry(
                                subtask.id,
                                &subtask.title,
                                "completed",
                                &attempts,
                                &supervisor_entries,
                            ));
                            continue 'subtask;
                        }
                        GateDecision::Ship { arbiter_entry } => {
                            completed.push(subtask.id);
                            let mut entry = build_subtask_entry(
                                subtask.id,
                                &subtask.title,
                                "completed",
                                &attempts,
                                &supervisor_entries,
                            );
                            if let Some(obj) = entry.as_object_mut() {
                                obj.insert("arbiter".to_string(), arbiter_entry);
                            }
                            subtask_entries.push(entry);
                            continue 'subtask;
                        }
                        GateDecision::Rework { findings } => {
                            // Not yet exhausted — rework with review findings.
                            let history = mem_history(mem_on, &attempts, hist_cap);
                            current_prompt = prompts::conduct_rework(
                                &original_prompt,
                                &changes,
                                &findings,
                                history.as_deref(),
                            );
                            continue;
                        }
                        GateDecision::Fail {
                            reason,
                            arbiter_entry,
                        } => {
                            failed = Some(reason);
                            let mut entry = build_subtask_entry(
                                subtask.id,
                                &subtask.title,
                                "failed",
                                &attempts,
                                &supervisor_entries,
                            );
                            if let Some(arb) = arbiter_entry {
                                if let Some(obj) = entry.as_object_mut() {
                                    obj.insert("arbiter".to_string(), arb);
                                }
                            }
                            subtask_entries.push(entry);
                            break 'subtask;
                        }
                    }
                }
                EvalDecision::Fail => {
                    failed = Some(format!(
                        "subtask {} failed: {}",
                        subtask.id, evaluation.feedback
                    ));
                    subtask_entries.push(build_subtask_entry(
                        subtask.id,
                        &subtask.title,
                        "failed",
                        &attempts,
                        &supervisor_entries,
                    ));
                    break 'subtask;
                }
                EvalDecision::Rework => {
                    if attempt_num >= MAX_REWORKS as usize || stalled {
                        // Exhausted, OR stalled: this attempt reproduced a prior
                        // attempt's exact diff + verify result (P1.5 circuit
                        // breaker) — more rework would only spin. Either way the
                        // subtask was not going to converge, so fail it.
                        let why = if stalled {
                            format!(
                                "stalled after {} attempt(s) — no progress (identical diff + verify): {}",
                                attempt_num + 1,
                                evaluation.feedback
                            )
                        } else {
                            format!(
                                "exhausted {} rework attempts: {}",
                                MAX_REWORKS, evaluation.feedback
                            )
                        };
                        failed = Some(format!("subtask {} {}", subtask.id, why));
                        subtask_entries.push(build_subtask_entry(
                            subtask.id,
                            &subtask.title,
                            "failed",
                            &attempts,
                            &supervisor_entries,
                        ));
                        break 'subtask;
                    }
                    // Prepare rework for the next iteration.
                    let history = mem_history(mem_on, &attempts, hist_cap);
                    current_prompt = prompts::conduct_rework(
                        &original_prompt,
                        &changes,
                        &evaluation.feedback,
                        history.as_deref(),
                    );
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
        "conductor_memory": {
            "enabled": mem_on,
            "ledger_char_cap": ledger_cap,
            "attempt_history_char_cap": hist_cap,
        },
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

/// Recited progress for the supervisor (Finding P1.9, lost-in-the-middle /
/// context-rot): the original task is pinned at the TOP, followed by a full
/// plan checklist with each subtask's status ([x] done, [>] current, [ ]
/// pending), then the bulk (latest changes) last. Reciting the goal + checklist
/// at the prompt edge keeps the model anchored as the run grows.
fn build_progress(
    task: &str,
    all: &[Subtask],
    completed: &[u32],
    current: &Subtask,
    changes: &str,
) -> String {
    let mut checklist = String::new();
    for st in all {
        let mark = if completed.contains(&st.id) {
            "[x]"
        } else if st.id == current.id {
            "[>]"
        } else {
            "[ ]"
        };
        // Titles are optional in the plan schema; fall back to a prompt slice so
        // the checklist is never a bare id.
        let label = if st.title.is_empty() {
            st.prompt.chars().take(48).collect::<String>()
        } else {
            st.title.clone()
        };
        let suffix = if st.id == current.id {
            "  ← current"
        } else {
            ""
        };
        checklist.push_str(&format!("{mark} {}. {label}{suffix}\n", st.id));
    }
    let changes_preview: String = changes.chars().take(500).collect();
    format!(
        "TASK (keep this in view):\n{task}\n\nPLAN CHECKLIST:\n{checklist}\nLatest changes to the current subtask (preview):\n{changes_preview}"
    )
}

/// What the review/arbiter gate decided for an accepted subtask. Extracted from
/// run_conduct's hot loop so the gate logic stays shallow (the loop just acts on
/// this) instead of nesting ~18 levels deep inside the Accept arm.
enum GateDecision {
    /// Review clean, reviewer unavailable, or no reviewer → accept the subtask.
    Accept,
    /// Review blocked but the arbiter shipped on exhaustion → accept; embed this
    /// arbiter record in the completed entry.
    Ship { arbiter_entry: serde_json::Value },
    /// Review blocked and reworks remain → rework with these findings.
    Rework { findings: String },
    /// Terminal failure (exhausted with no arbiter, arbiter Fail, or arbiter
    /// unavailable). `arbiter_entry` is the record to embed, if any.
    Fail {
        reason: String,
        arbiter_entry: Option<serde_json::Value>,
    },
}

/// Patch the most recent attempt with the review status (+ cross-family marker),
/// mirroring the inline patch the gate used before extraction.
fn patch_review_status(
    attempts: &mut [serde_json::Value],
    review_status: &str,
    cf_marker: Option<&str>,
) {
    if let Some(obj) = attempts.last_mut().and_then(|a| a.as_object_mut()) {
        obj.insert(
            "review".to_string(),
            serde_json::Value::String(review_status.to_string()),
        );
        if let Some(m) = cf_marker {
            obj.insert(
                "cross_family".to_string(),
                serde_json::Value::String(m.to_string()),
            );
        }
    }
}

/// The review gate (plus the arbiter on rework exhaustion) for an accepted
/// subtask. Advisory: a transient reviewer failure degrades to Accept; a
/// transient arbiter failure fails closed. Returns the decision for the loop to
/// act on, keeping the loop body flat.
#[allow(clippy::too_many_arguments)]
async fn run_review_gate(
    changes: &str,
    subtask: &Subtask,
    reviewer: &RoleHandle,
    arbiter: Option<&RoleHandle>,
    cross_family: bool,
    workers: &[CouncilMember],
    worker_provider: crate::event::Provider,
    attempt_num: usize,
    attempts: &mut Vec<serde_json::Value>,
    all_fallbacks: &mut Vec<Fallback>,
    ledger: Option<&str>,
    mem_on: bool,
    hist_cap: usize,
    quota: &QuotaStore,
    health: &ModelHealth,
    cwd: &std::path::Path,
    timeout: Duration,
) -> GateDecision {
    // Cross-family review (Finding 7): front the reviewer with a different family
    // than the worker that produced the diff.
    let (review_ladder, cf_marker): (Vec<Rung>, Option<&str>) = if cross_family {
        let (l, degraded) = cross_family_ladder(&reviewer.ladder, workers, worker_provider);
        (
            l,
            Some(if degraded {
                "degraded_same_family"
            } else {
                "applied"
            }),
        )
    } else {
        (reviewer.ladder.clone(), None)
    };

    let (review_result, rev_fallbacks) = match super::review::run_review_ladder(
        changes,
        &review_ladder,
        health,
        quota,
        cwd.to_path_buf(),
        timeout,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            // Advisory: a transient reviewer failure must not abort the run. The
            // diff already passed the conductor + grounding, so accept this round.
            tracing::warn!(error = %e, "reviewer unavailable; accepting subtask this round without review");
            patch_review_status(attempts, "unavailable", None);
            return GateDecision::Accept;
        }
    };
    all_fallbacks.extend(rev_fallbacks);

    let review_blocks = match &review_result.verdict {
        Some(v) => v.has_critical(),
        None => true, // unparseable → fail-closed
    };
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
    patch_review_status(attempts, review_status, cf_marker);

    if !review_blocks {
        return GateDecision::Accept;
    }
    if attempt_num < MAX_REWORKS as usize {
        return GateDecision::Rework {
            findings: findings_text,
        };
    }

    // Exhausted with review still blocking → arbiter (if configured).
    let Some(arb) = arbiter else {
        return GateDecision::Fail {
            reason: format!(
                "subtask {} exhausted {} rework attempts (review gate): {}",
                subtask.id, MAX_REWORKS, findings_text
            ),
            arbiter_entry: None,
        };
    };

    let arb_history = mem_history(mem_on, attempts.as_slice(), hist_cap);
    // Cross-family arbiter (Finding 7): same rule as the reviewer.
    let arb_ladder = if cross_family {
        cross_family_ladder(&arb.ladder, workers, worker_provider).0
    } else {
        arb.ladder.clone()
    };
    let arb_result = {
        let prompt = super::prompts::arbiter_decide(
            &subtask.prompt,
            changes,
            &findings_text,
            ledger,
            arb_history.as_deref(),
        );
        let cwd2 = cwd.to_path_buf();
        run_with_failover(
            &arb_ladder,
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
        .await
    };
    match arb_result {
        Ok(arb_fo) => {
            all_fallbacks.extend(arb_fo.fallbacks);
            // Fail-safe: unparseable → Fail.
            let arb_verdict = parse_arbiter(&arb_fo.outcome.final_text).unwrap_or(ArbiterVerdict {
                decision: ArbiterDecision::Fail,
                reason: "arbiter output unparseable".to_string(),
            });
            if arb_verdict.decision == ArbiterDecision::Ship {
                GateDecision::Ship {
                    arbiter_entry: serde_json::json!({
                        "decision": "ship",
                        "reason": arb_verdict.reason,
                    }),
                }
            } else {
                GateDecision::Fail {
                    reason: format!(
                        "subtask {} arbiter failed: {}",
                        subtask.id, arb_verdict.reason
                    ),
                    arbiter_entry: Some(serde_json::json!({
                        "decision": "fail",
                        "reason": arb_verdict.reason,
                    })),
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "arbiter unavailable; failing closed (not shipping un-arbitrated)");
            GateDecision::Fail {
                reason: format!("subtask {} arbiter unavailable: {e}", subtask.id),
                arbiter_entry: Some(serde_json::json!({
                    "decision": "unavailable",
                    "reason": format!("arbiter unavailable: {e}"),
                })),
            }
        }
    }
}

/// Cross-family review ladder (Finding 7): a reviewer/arbiter ladder whose front
/// is a DIFFERENT model family than `worker_provider`. Order: the role's own
/// different-family rungs, then different-family worker primaries (borrowed as
/// fallbacks so a single-rung same-family reviewer still gets a cross-family
/// option), then the role's same-family rungs last (fail-open if the
/// cross-family models are all dead at runtime). Returns `(ladder, degraded)`,
/// where `degraded` = no different-family option existed at all (front is
/// same-family) — the review still runs, just same-family, and is marked.
fn cross_family_ladder(
    role_ladder: &[Rung],
    workers: &[CouncilMember],
    worker_provider: crate::event::Provider,
) -> (Vec<Rung>, bool) {
    let mut out: Vec<Rung> = role_ladder
        .iter()
        .filter(|r| r.candidate.provider != worker_provider)
        .cloned()
        .collect();
    for w in workers {
        if let Some(first) = w.ladder.first() {
            if first.candidate.provider != worker_provider {
                out.push(first.clone());
            }
        }
    }
    for r in role_ladder
        .iter()
        .filter(|r| r.candidate.provider == worker_provider)
    {
        out.push(r.clone());
    }
    let degraded = out
        .first()
        .map(|r| r.candidate.provider == worker_provider)
        .unwrap_or(true);
    (out, degraded)
}

// ─── ConductorMemory: ledger + attempt history ───────────────────────────────

const LEDGER_ELIDED: &str = "(… earlier subtasks elided)";
const HISTORY_ELIDED: &str = "(… earlier attempts elided)";

/// The verify status of the most recent attempt, for the mechanical summary.
fn last_verify(attempts: &[serde_json::Value]) -> &str {
    attempts
        .last()
        .and_then(|a| a.get("verify"))
        .and_then(|v| v.as_str())
        .unwrap_or("-")
}

/// THE single place a finished-subtask transcript entry is built, so `status` +
/// `summary` can never be forgotten at one of the many terminal sites. `summary`
/// is MECHANICAL ONLY (status + verify digest) — no worker/feedback text leaks
/// into the cross-subtask ledger. Arbiter sites insert an extra `arbiter` field
/// on the returned object before pushing.
fn build_subtask_entry(
    id: u32,
    title: &str,
    status: &str,
    attempts: &[serde_json::Value],
    supervisor: &[serde_json::Value],
) -> serde_json::Value {
    let summary = format!("{status} (verify: {})", last_verify(attempts));
    serde_json::json!({
        "id": id,
        "title": title,
        "status": status,
        "summary": summary,
        "attempts": attempts,
        "supervisor": supervisor,
    })
}

/// Char-boundary-safe prefix truncation (keep the head).
fn truncate_head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// Fold lines into at most `cap` chars, keeping the MOST-RECENT lines (the tail
/// of `lines`). If any are dropped, prepend `marker` — counted inside the budget
/// so the rendered block never exceeds `cap`.
fn fold_lines(lines: &[String], cap: usize, marker: &str) -> String {
    let full = lines.join("\n");
    if full.len() <= cap {
        return full;
    }
    // Pathologically small cap: not even the marker fits. Return a bounded marker
    // so the rendered block is still guaranteed `<= cap` (never panics, never leaks).
    if cap < marker.len() + 1 {
        return truncate_head(marker, cap).to_string();
    }
    let budget = cap.saturating_sub(marker.len() + 1); // +1 for the marker's newline
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for line in lines.iter().rev() {
        let add = line.len() + if kept.is_empty() { 0 } else { 1 };
        if used + add > budget {
            break;
        }
        used += add;
        kept.push(line.as_str());
    }
    kept.reverse();
    if kept.is_empty() {
        // Even the single most-recent line overflows: keep a bounded prefix.
        let last = lines.last().map(|s| s.as_str()).unwrap_or("");
        return format!("{marker}\n{}", truncate_head(last, budget));
    }
    format!("{marker}\n{}", kept.join("\n"))
}

/// Folded plan ledger of prior FINISHED subtasks (the entries already pushed).
/// `None` when there are none. Mechanical content only (id/title/status/summary).
fn render_ledger(prior_entries: &[serde_json::Value], cap: usize) -> Option<String> {
    if prior_entries.is_empty() {
        return None;
    }
    let lines: Vec<String> = prior_entries
        .iter()
        .map(|e| {
            let id = e.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            let title = e.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let summary = e.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            format!("- subtask {id} \"{title}\": {summary}")
        })
        .collect();
    Some(fold_lines(&lines, cap, LEDGER_ELIDED))
}

/// Folded attempt history for the current subtask. `None` when there are no
/// prior attempts. Carries the conductor's own feedback so it stops repeating
/// itself; the caller's template XML-isolates it.
fn render_attempt_history(attempts: &[serde_json::Value], cap: usize) -> Option<String> {
    if attempts.is_empty() {
        return None;
    }
    let lines: Vec<String> = attempts
        .iter()
        .map(|a| {
            let n = a.get("attempt").and_then(|v| v.as_u64()).unwrap_or(0);
            let decision = a.get("decision").and_then(|v| v.as_str()).unwrap_or("");
            let verify = a.get("verify").and_then(|v| v.as_str()).unwrap_or("");
            let feedback = a.get("feedback").and_then(|v| v.as_str()).unwrap_or("");
            format!("- attempt {n}: {decision} (verify: {verify}) — {feedback}")
        })
        .collect();
    Some(fold_lines(&lines, cap, HISTORY_ELIDED))
}

/// Memory-gated ledger render: `None` when memory is off.
fn mem_ledger(on: bool, prior_entries: &[serde_json::Value], cap: usize) -> Option<String> {
    if on {
        render_ledger(prior_entries, cap)
    } else {
        None
    }
}

/// Memory-gated attempt-history render: `None` when memory is off.
fn mem_history(on: bool, attempts: &[serde_json::Value], cap: usize) -> Option<String> {
    if on {
        render_attempt_history(attempts, cap)
    } else {
        None
    }
}

const BLACKBOARD_ELIDED: &str = "(… earlier subtasks elided)";

/// Worker-facing blackboard: a mechanical roster of prior FINISHED subtasks
/// (status only — NO verify digest, NO feedback) plus the files modified this
/// run. `None` when there is nothing to show. Mechanical content only — this is
/// the read-only subset workers may see (vs. the conductor's richer ledger).
fn render_blackboard(
    prior_entries: &[serde_json::Value],
    changed_files: &[String],
    cap: usize,
) -> Option<String> {
    if prior_entries.is_empty() && changed_files.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = prior_entries
        .iter()
        .map(|e| {
            let id = e.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            let title = e.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let status = e.get("status").and_then(|v| v.as_str()).unwrap_or("");
            format!("- subtask {id} \"{title}\": {status}")
        })
        .collect();
    if !changed_files.is_empty() {
        lines.push(format!(
            "- files modified this run: {}",
            changed_files.join(", ")
        ));
    }
    Some(fold_lines(&lines, cap, BLACKBOARD_ELIDED))
}

/// Memory-gated blackboard render: `None` when memory is off.
fn mem_blackboard(
    on: bool,
    prior_entries: &[serde_json::Value],
    changed_files: &[String],
    cap: usize,
) -> Option<String> {
    if on {
        render_blackboard(prior_entries, changed_files, cap)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(id: u32, title: &str, prompt: &str) -> Subtask {
        Subtask {
            id,
            title: title.into(),
            prompt: prompt.into(),
            depends_note: String::new(),
        }
    }

    #[test]
    fn build_progress_recites_task_and_plan_checklist() {
        let plan = vec![
            st(1, "Set up", "p1"),
            st(2, "Wire it", "p2"),
            st(3, "Test", "p3"),
        ];
        let progress = build_progress("Build the thing", &plan, &[1], &plan[1], "diff here");
        assert!(
            progress.contains("TASK (keep this in view):\nBuild the thing"),
            "task pinned at top: {progress}"
        );
        assert!(
            progress.contains("[x] 1. Set up"),
            "done marker: {progress}"
        );
        assert!(
            progress.contains("[>] 2. Wire it  ← current"),
            "current marker: {progress}"
        );
        assert!(
            progress.contains("[ ] 3. Test"),
            "pending marker: {progress}"
        );
        assert!(
            progress.contains("diff here"),
            "changes preview last: {progress}"
        );
    }

    #[test]
    fn build_progress_falls_back_to_prompt_when_title_empty() {
        let plan = vec![st(1, "", "do the long thing here")];
        let progress = build_progress("T", &plan, &[], &plan[0], "");
        assert!(
            progress.contains("[>] 1. do the long thing here  ← current"),
            "empty title falls back to prompt slice: {progress}"
        );
    }

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

    // ── ConductorMemory renderers ───────────────────────────────────────────

    #[test]
    fn render_ledger_and_history_empty_are_none() {
        assert!(render_ledger(&[], 100).is_none());
        assert!(render_attempt_history(&[], 100).is_none());
    }

    #[test]
    fn build_subtask_entry_summary_is_mechanical_only() {
        // Feedback with sensitive text must NOT reach the cross-subtask ledger.
        let attempts = vec![serde_json::json!({
            "attempt": 0, "worker": "w", "decision": "accept",
            "feedback": "SECRET worker feedback text", "changes_chars": 3, "verify": "passed",
        })];
        let entry = build_subtask_entry(1, "title", "completed", &attempts, &[]);
        assert_eq!(entry["status"], "completed");
        assert_eq!(entry["summary"], "completed (verify: passed)");
        assert!(
            !entry["summary"].as_str().unwrap().contains("SECRET"),
            "no feedback text may leak into the ledger summary"
        );
    }

    #[test]
    fn last_verify_uses_the_most_recent_attempt() {
        let attempts = vec![
            serde_json::json!({"attempt":0,"decision":"rework","verify":"failed","feedback":"x"}),
            serde_json::json!({"attempt":1,"decision":"accept","verify":"passed","feedback":""}),
        ];
        assert_eq!(last_verify(&attempts), "passed");
        assert_eq!(last_verify(&[]), "-");
    }

    #[test]
    fn render_ledger_folds_under_cap_with_marker() {
        let entries: Vec<serde_json::Value> = (1..=20)
            .map(|i| build_subtask_entry(i, "t", "completed", &[], &[]))
            .collect();
        let cap = 120;
        let rendered = render_ledger(&entries, cap).unwrap();
        assert!(
            rendered.len() <= cap,
            "rendered ledger {} exceeds cap {cap}",
            rendered.len()
        );
        assert!(
            rendered.contains(LEDGER_ELIDED),
            "an elided ledger must carry the marker"
        );
        assert!(
            rendered.contains("subtask 20"),
            "the most-recent subtask must be kept"
        );
    }

    #[test]
    fn render_ledger_fits_uses_all_lines_no_marker() {
        let entries: Vec<serde_json::Value> = (1..=2)
            .map(|i| build_subtask_entry(i, "t", "completed", &[], &[]))
            .collect();
        let rendered = render_ledger(&entries, 1000).unwrap();
        assert!(!rendered.contains(LEDGER_ELIDED));
        assert!(rendered.contains("subtask 1"));
        assert!(rendered.contains("subtask 2"));
    }

    #[test]
    fn render_attempt_history_includes_feedback_and_decision() {
        let attempts = vec![
            serde_json::json!({"attempt":0,"decision":"rework","verify":"failed","feedback":"add docs"}),
        ];
        let h = render_attempt_history(&attempts, 500).unwrap();
        assert!(h.contains("attempt 0"));
        assert!(h.contains("rework"));
        assert!(h.contains("add docs"));
    }

    #[test]
    fn mem_gates_return_none_when_off() {
        let entries = vec![build_subtask_entry(1, "t", "completed", &[], &[])];
        assert!(mem_ledger(false, &entries, 100).is_none());
        assert!(mem_history(false, &entries, 100).is_none());
        assert!(mem_ledger(true, &entries, 100).is_some());
    }

    #[test]
    fn fold_lines_bounds_even_tiny_cap() {
        let lines = vec!["aaaa".to_string(), "bbbb".to_string(), "cccc".to_string()];
        // cap smaller than the marker → output is a bounded marker, still <= cap.
        for cap in [1usize, 3, 5, 10] {
            let out = fold_lines(&lines, cap, LEDGER_ELIDED);
            assert!(out.len() <= cap, "fold output {out:?} exceeds cap {cap}");
        }
    }

    #[test]
    fn render_attempt_history_respects_cap() {
        let attempts: Vec<serde_json::Value> = (0..30)
            .map(|n| {
                serde_json::json!({
                    "attempt": n, "decision": "rework", "verify": "failed",
                    "feedback": "some moderately long feedback text here",
                })
            })
            .collect();
        let cap = 200;
        let h = render_attempt_history(&attempts, cap).unwrap();
        assert!(h.len() <= cap, "history {} exceeds cap {cap}", h.len());
        assert!(h.contains(HISTORY_ELIDED));
        assert!(h.contains("attempt 29"), "most-recent attempt must be kept");
    }

    #[test]
    fn render_blackboard_empty_is_none() {
        assert!(render_blackboard(&[], &[], 100).is_none());
    }

    #[test]
    fn render_blackboard_files_only_when_no_prior_subtasks() {
        // No prior subtasks but files already changed this run → still Some.
        let b = render_blackboard(&[], &["src/lib.rs".to_string()], 200).unwrap();
        assert!(b.contains("files modified this run: src/lib.rs"));
        assert!(!b.contains("subtask"));
    }

    #[test]
    fn render_blackboard_is_status_only_and_lists_files() {
        // An entry whose attempts carry sensitive feedback + a verify digest in
        // its summary — neither may surface in the worker blackboard.
        let entry = build_subtask_entry(
            1,
            "mathops",
            "completed",
            &[serde_json::json!({
                "attempt": 0, "decision": "accept", "verify": "passed",
                "feedback": "SECRET conductor note",
            })],
            &[],
        );
        let files = vec!["src/mathops.rs".to_string()];
        let b = render_blackboard(&[entry], &files, 500).unwrap();
        assert!(b.contains("- subtask 1 \"mathops\": completed"));
        assert!(b.contains("files modified this run: src/mathops.rs"));
        // mechanical only: no verify digest, no feedback prose
        assert!(
            !b.contains("verify:"),
            "blackboard must not carry the verify digest"
        );
        assert!(!b.contains("SECRET"), "blackboard must not carry feedback");
    }

    #[test]
    fn mem_blackboard_off_is_none() {
        let entries = vec![build_subtask_entry(1, "t", "completed", &[], &[])];
        let files = vec!["a".to_string()];
        assert!(mem_blackboard(false, &entries, &files, 100).is_none());
        assert!(mem_blackboard(true, &entries, &files, 100).is_some());
    }

    // ── cross-family review ──────────────────────────────────────────────────
    struct StubAdapter;
    impl crate::adapters::Adapter for StubAdapter {
        fn provider(&self) -> crate::event::Provider {
            crate::event::Provider::Claude
        }
        fn cli_binary(&self) -> &'static str {
            "stub"
        }
        fn build_command(&self, _req: &crate::adapters::RunRequest) -> tokio::process::Command {
            tokio::process::Command::new("true")
        }
        fn parse_line(&self, _line: &str) -> Vec<crate::event::AgentEvent> {
            Vec::new()
        }
    }
    fn cf_rung(p: crate::event::Provider, model: &str) -> Rung {
        Rung {
            candidate: crate::config::ModelCandidate {
                provider: p,
                model: model.into(),
            },
            adapter: std::sync::Arc::new(StubAdapter),
        }
    }
    fn cf_member(p: crate::event::Provider, label: &str) -> CouncilMember {
        CouncilMember {
            label: label.into(),
            ladder: vec![cf_rung(p, "m")],
        }
    }

    #[test]
    fn cross_family_fronts_a_different_family() {
        use crate::event::Provider;
        // reviewer = single Codex rung; worker = Codex; pool has a Gemini worker.
        let (l, degraded) = cross_family_ladder(
            &[cf_rung(Provider::Codex, "rev")],
            &[
                cf_member(Provider::Codex, "w1"),
                cf_member(Provider::Gemini, "w2"),
            ],
            Provider::Codex,
        );
        assert!(!degraded);
        assert_eq!(
            l[0].candidate.provider,
            Provider::Gemini,
            "front must be cross-family"
        );
        assert!(
            l.iter().any(|r| r.candidate.provider == Provider::Codex),
            "same-family reviewer kept as a fallback (fail-open)"
        );
    }

    #[test]
    fn cross_family_degrades_when_single_family() {
        use crate::event::Provider;
        let (l, degraded) = cross_family_ladder(
            &[cf_rung(Provider::Codex, "rev")],
            &[cf_member(Provider::Codex, "w1")],
            Provider::Codex,
        );
        assert!(degraded, "no other family available → degraded");
        assert_eq!(
            l[0].candidate.provider,
            Provider::Codex,
            "still runs same-family (fail-open, never blocks the review)"
        );
    }

    #[test]
    fn cross_family_noop_when_reviewer_already_different() {
        use crate::event::Provider;
        // reviewer = Claude, worker = Codex → already cross-family.
        let (l, degraded) = cross_family_ladder(
            &[cf_rung(Provider::Claude, "rev")],
            &[cf_member(Provider::Codex, "w1")],
            Provider::Codex,
        );
        assert!(!degraded);
        assert_eq!(l[0].candidate.provider, Provider::Claude);
    }

    #[test]
    fn cross_family_reorders_multi_rung_stably() {
        use crate::event::Provider;
        // A multi-family reviewer ladder; worker = Codex. Different-family rungs
        // move to the front (stable), the same-family rung is appended last.
        let ladder = vec![
            cf_rung(Provider::Codex, "a"),
            cf_rung(Provider::Claude, "b"),
            cf_rung(Provider::Gemini, "c"),
        ];
        let (l, degraded) = cross_family_ladder(&ladder, &[], Provider::Codex);
        assert!(!degraded);
        assert_eq!(
            l[0].candidate.provider,
            Provider::Claude,
            "stable: Claude first"
        );
        assert_eq!(l[1].candidate.provider, Provider::Gemini, "then Gemini");
        assert_eq!(
            l.last().unwrap().candidate.provider,
            Provider::Codex,
            "same-family appended last (fail-open)"
        );
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
            None,
            None,
        );
        assert!(parse_evaluation(&p).is_some());
    }

    #[test]
    fn supervisor_template_example_parses() {
        let p = crate::orchestrator::prompts::supervisor_gate("task", "progress", None, None);
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
        let p = crate::orchestrator::prompts::arbiter_decide(
            "subtask", "changes", "findings", None, None,
        );
        let v = parse_arbiter(&p).expect("arbiter template example must parse");
        // Template example decision is "ship".
        assert_eq!(v.decision, ArbiterDecision::Ship);
    }
}
