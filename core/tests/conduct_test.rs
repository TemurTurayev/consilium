mod common;

#[allow(unused_imports)]
use common::{RecordingAdapter, RecordingSequenced, ScriptedAdapter, SequencedAdapter};
use consilium::config::{ConductorMemoryConfig, ModelCandidate, VerifyConfig};
use consilium::event::Provider;
use consilium::orchestrator::conduct::{run_conduct, ConductDeps, ConductOutcome, RoleHandle};
use consilium::orchestrator::council::CouncilMember;
use consilium::orchestrator::resilience::{ModelHealth, Rung};
use consilium::quota::QuotaStore;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ─── helpers ────────────────────────────────────────────────────────────────

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn temp_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["commit", "--allow-empty", "-m", "init", "-q"]);
    dir
}

/// Build an N-subtask plan JSON string from (id, title, prompt) triples.
fn plan_json(subtasks: &[(u32, &str, &str)]) -> String {
    let entries: Vec<String> = subtasks
        .iter()
        .map(|(id, title, prompt)| {
            format!(r#"{{"id":{id},"title":"{title}","prompt":"{prompt}","depends_note":""}}"#)
        })
        .collect();
    format!(r#"{{"subtasks":[{}]}}"#, entries.join(","))
}

/// Like `plan_json` but each subtask also carries an explicit `depends_on` edge list.
fn plan_json_with_deps(subtasks: &[(u32, &str, &str, &[u32])]) -> String {
    let entries: Vec<String> = subtasks
        .iter()
        .map(|(id, title, prompt, deps)| {
            let deps_csv = deps.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(",");
            format!(
                r#"{{"id":{id},"title":"{title}","prompt":"{prompt}","depends_note":"","depends_on":[{deps_csv}]}}"#
            )
        })
        .collect();
    format!(r#"{{"subtasks":[{}]}}"#, entries.join(","))
}

fn accept_json() -> String {
    r#"{"decision":"accept","feedback":""}"#.to_string()
}

fn rework_json(feedback: &str) -> String {
    format!(r#"{{"decision":"rework","feedback":"{feedback}"}}"#)
}

fn fail_json(feedback: &str) -> String {
    format!(r#"{{"decision":"fail","feedback":"{feedback}"}}"#)
}

#[allow(dead_code)]
fn supervisor_ok_json() -> String {
    r#"{"status":"ok","note":""}"#.to_string()
}

fn supervisor_halt_json(note: &str) -> String {
    format!(r#"{{"status":"halt","note":"{note}"}}"#)
}

fn supervisor_concern_json(note: &str) -> String {
    format!(r#"{{"status":"concern","note":"{note}"}}"#)
}

/// Reviewer verdict with one critical finding.
fn review_critical_json(file: &str, description: &str) -> String {
    format!(
        r#"{{"findings":[{{"severity":"critical","file":"{file}","description":"{description}"}}]}}"#
    )
}

/// Reviewer verdict with no findings (clean).
fn review_clean_json() -> String {
    r#"{"findings":[]}"#.to_string()
}

fn arbiter_ship_json(reason: &str) -> String {
    format!(r#"{{"decision":"ship","reason":"{reason}"}}"#)
}

fn arbiter_fail_json(reason: &str) -> String {
    format!(r#"{{"decision":"fail","reason":"{reason}"}}"#)
}

fn store() -> QuotaStore {
    QuotaStore::open_in_memory().unwrap()
}

/// Wrap a single adapter in a one-rung CouncilMember ladder.
fn solo_worker(
    label: &str,
    provider: Provider,
    model: &str,
    adapter: Arc<dyn consilium::adapters::Adapter>,
) -> CouncilMember {
    CouncilMember {
        label: label.into(),
        ladder: vec![Rung {
            candidate: ModelCandidate {
                provider,
                model: model.into(),
            },
            adapter,
        }],
    }
}

/// Wrap a single adapter in a one-rung RoleHandle ladder.
fn solo_role_handle(
    provider: Provider,
    model: &str,
    adapter: Arc<dyn consilium::adapters::Adapter>,
) -> RoleHandle {
    RoleHandle {
        ladder: vec![Rung {
            candidate: ModelCandidate {
                provider,
                model: model.into(),
            },
            adapter,
        }],
    }
}

fn health() -> ModelHealth {
    ModelHealth::new()
}

const TIMEOUT: Duration = Duration::from_secs(30);

// ─── Test 1: happy_path_single_subtask ─────────────────────────────────────

#[tokio::test]
async fn happy_path_single_subtask() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: step 0 = plan, step 1 = accept
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "create file", "create out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    // Worker: writes out.txt
    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo 'hello' > out.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "created out.txt")
    });

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome: ConductOutcome = run_conduct(
        "create a file",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    assert!(outcome.halted.is_none());
    assert!(outcome.failed.is_none());
    assert!(
        repo.path().join("out.txt").exists(),
        "worker should have created out.txt"
    );

    // Transcript: 1 subtask entry with 1 attempt
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let attempts = entries[0]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["decision"], "accept");
    assert_eq!(attempts[0]["worker"], "codex-worker");
    assert!(attempts[0]["changes_chars"].as_u64().unwrap() > 0);
}

// ─── Test 2: rework_then_accept ────────────────────────────────────────────

#[tokio::test]
async fn rework_then_accept() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: plan → rework("add more") → accept
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "write file", "write content")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &rework_json("add more content")),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    // Worker: step 0 writes "v1", step 1 appends "v2"
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo 'v1' > work.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "wrote v1")
            },
            ScriptedAdapter {
                pre_script: "echo 'v2' >> work.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "appended v2")
            },
        ],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "write content",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    assert!(outcome.failed.is_none());

    // Transcript: 2 attempts with decisions rework → accept
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    let attempts = entries[0]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["decision"], "rework");
    assert_eq!(attempts[1]["decision"], "accept");
}

// ─── Test 3: rework_exhaustion_fails ───────────────────────────────────────

#[tokio::test]
async fn rework_exhaustion_fails() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: plan → rework → rework → rework (MAX_REWORKS=2 → exhausted after 3rd rework)
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "do thing", "do it")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &rework_json("not good enough")),
            ScriptedAdapter::ok_with_text(Provider::Claude, &rework_json("still not good")),
            ScriptedAdapter::ok_with_text(Provider::Claude, &rework_json("still failing")),
        ],
    ));

    // Distinct diffs each round (different file content) so this exercises
    // genuine rework EXHAUSTION, not the P1.5 stall breaker (which trips only on
    // a repeated diff+verify fingerprint).
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo attempt-a > work.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "tried a")
            },
            ScriptedAdapter {
                pre_script: "echo attempt-b > work.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "tried b")
            },
            ScriptedAdapter {
                pre_script: "echo attempt-c > work.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "tried c")
            },
        ],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert!(
        outcome.failed.is_some(),
        "should fail after MAX_REWORKS exhausted"
    );
    assert!(outcome.completed.is_empty());
    // Exactly MAX_REWORKS + 1 attempts: initial + 2 reworks, then exhaustion.
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    assert_eq!(entries[0]["attempts"].as_array().unwrap().len(), 3);
}

// ─── Test 3b: rework_stall_breaks_early (P1.5 stagnation) ───────────────────
// A worker that reproduces the SAME diff + verify every attempt makes no
// progress; the stagnation circuit breaker stops early instead of burning the
// full rework budget.
#[tokio::test]
async fn rework_stall_breaks_early() {
    let repo = temp_repo();
    let quota = store();

    // plan → rework → rework. Attempt 1 repeats attempt 0's exact fingerprint, so
    // the breaker fires after the first repeat — only 2 evals are ever needed.
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "do thing", "do it")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &rework_json("nope")),
            ScriptedAdapter::ok_with_text(Provider::Claude, &rework_json("still nope")),
        ],
    ));

    // Same output + no file mutation every attempt → identical diff + verify.
    let worker = Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "did thing"));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    let failed = outcome.failed.expect("a stalled subtask fails the run");
    assert!(
        failed.contains("stalled"),
        "should report a stall: {failed}"
    );
    assert!(outcome.completed.is_empty());
    // Initial + 1 rework, then the repeat trips the breaker — fewer than the 3
    // attempts a full MAX_REWORKS exhaustion records.
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    assert_eq!(
        entries[0]["attempts"].as_array().unwrap().len(),
        2,
        "stall stops after the first repeated fingerprint"
    );
}

#[tokio::test]
async fn replan_rescues_a_failed_run() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "first try", "write failed.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &fail_json("needs a new plan")),
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(2, "replanned", "write recovered.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo failed > failed.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "wrote failed.txt")
            },
            ScriptedAdapter {
                pre_script: "echo recovered > recovered.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "wrote recovered.txt")
            },
        ],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 1,
        budget: None,
    };

    let outcome = run_conduct(
        "write a file",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert!(outcome.failed.is_none(), "replan should rescue the run");
    assert_eq!(outcome.completed, vec![2]);
    assert!(repo.path().join("recovered.txt").exists());
    assert_eq!(
        outcome.transcript["replans"].as_array().unwrap().len(),
        1,
        "transcript should record exactly one replan"
    );
}

#[tokio::test]
async fn replan_with_cross_plan_depends_on_succeeds() {
    let repo = temp_repo();
    let quota = store();
    // Pass 1: subtask 1 completes, subtask 2 fails → replan. Pass 2: subtask 3
    // depends_on [1] — a COMPLETED id from the prior plan — must be accepted as a
    // satisfied edge, not rejected as "invalid plan: ... unknown subtask 1".
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json_with_deps(&[
                    (1, "ok", "create one.txt", &[]),
                    (2, "doomed", "create two.txt", &[]),
                ]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()), // eval subtask 1
            ScriptedAdapter::ok_with_text(Provider::Claude, &fail_json("subtask 2 wrong")), // eval subtask 2
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json_with_deps(&[(3, "build on 1", "create three.txt", &[1])]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()), // eval subtask 3
        ],
    ));
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo one > one.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "one")
            },
            ScriptedAdapter {
                pre_script: "echo two > two.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "two")
            },
            ScriptedAdapter {
                pre_script: "echo three > three.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "three")
            },
        ],
    ));
    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker,
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 1,
        budget: None,
    };
    let outcome = run_conduct(
        "cross-plan deps",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert!(
        outcome.completed.contains(&3),
        "replanned subtask depending on a completed id must run; got completed={:?} failed={:?}",
        outcome.completed,
        outcome.failed
    );
    assert!(
        repo.path().join("three.txt").exists(),
        "three.txt should exist"
    );
    assert!(
        !outcome
            .failed
            .as_deref()
            .unwrap_or("")
            .contains("invalid plan"),
        "must not spuriously reject the cross-plan dependency: {:?}",
        outcome.failed
    );
}

#[tokio::test]
async fn replan_disabled_by_default_still_fails() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "first try", "write failed.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &fail_json("needs a new plan")),
        ],
    ));

    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo failed > failed.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "wrote failed.txt")
    });

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "write a file",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert!(
        outcome.failed.is_some(),
        "run should still fail without replans"
    );
    assert!(
        outcome.transcript["replans"].as_array().unwrap().is_empty(),
        "replan should not be attempted when disabled"
    );
}

#[tokio::test]
async fn budget_trip_is_terminal_even_with_replans() {
    // A blown wall-clock budget must end the run even when replans are allowed —
    // never spend more conductor calls past the budget (reviewer-found interaction).
    let repo = temp_repo();
    let quota = store();
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![ScriptedAdapter::ok_with_text(
            Provider::Claude,
            &plan_json(&[(1, "a", "create a.txt"), (2, "b", "create b.txt")]),
        )],
    ));
    let worker = Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "noop"));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker,
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        budget: Some(Duration::from_millis(0)),
        max_replans: 1,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    let failed = outcome.failed.expect("budget trip fails the run");
    assert!(
        failed.contains("budget"),
        "the budget reason must survive (no replan overwrote it): {failed}"
    );
    assert!(
        outcome.transcript["replans"].as_array().unwrap().is_empty(),
        "a blown budget must not trigger a replan"
    );
}

// ─── Test 4: supervisor_halt_aborts ────────────────────────────────────────

#[tokio::test]
async fn supervisor_halt_aborts() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: plan → (supervisor halts before evaluation)
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "do thing", "do it")]),
            ),
            // Evaluation should NOT be called — supervisor halts first
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "done"));

    let supervisor = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &supervisor_halt_json("scope creep detected"),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: Some(solo_role_handle(Provider::Claude, "model", supervisor)),
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert!(
        outcome.halted.is_some(),
        "supervisor halt should set halted"
    );
    assert!(
        outcome
            .halted
            .as_deref()
            .unwrap_or("")
            .contains("scope creep"),
        "halt reason should mention scope creep"
    );
    assert!(outcome.completed.is_empty());
    // Transcript should record the halt
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    // Halt fires BEFORE evaluation — no attempt entry exists for the subtask.
    assert!(entries[0]["attempts"].as_array().unwrap().is_empty());
    let supervisor_entries = entries[0]["supervisor"].as_array().unwrap();
    assert!(!supervisor_entries.is_empty());
    assert_eq!(supervisor_entries[0]["status"], "halt");
}

// ─── Test 5: worker_all_rungs_failed_counts_as_attempt ────────────────────────
// A single-rung worker that always fails exhausts all rungs (run_with_failover
// retries Transient once; both attempts fail → Err). conduct treats that Err as
// a rework attempt. On the second conduct-loop iteration (attempt_num=1) the
// same worker's single rung now succeeds (SequencedAdapter advanced past the
// two failing responses used internally by the transient-retry), so the subtask
// eventually completes.

#[tokio::test]
async fn worker_failure_counts_as_attempt() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: plan → accept (after worker all-rungs-failed is treated as
    // a rework attempt; second loop iteration uses the same worker which now
    // succeeds).
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "do thing", "do it")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    // Worker: steps 0 and 1 fail (Transient → retry uses step 1 → still fails
    // → all 1 rung exhausted → Err). Step 2 succeeds and creates the file.
    // Conduct attempt_num=0: internal call 1 → step 0 fail, retry → step 1 fail
    //   → Err → rework attempt recorded in transcript.
    // Conduct attempt_num=1: internal call 1 → step 2 (ok) → succeeds.
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter::failing(Provider::Codex, "worker error: transient1"),
            ScriptedAdapter::failing(Provider::Codex, "worker error: transient2"),
            ScriptedAdapter {
                pre_script: "echo 'recovered' > recovered.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "recovered successfully")
            },
        ],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    assert!(outcome.failed.is_none());

    // Transcript: 2 attempts; first attempt feedback records the all-rungs-failed error
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    let attempts = entries[0]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    let first_feedback = attempts[0]["feedback"].as_str().unwrap_or("");
    assert!(
        first_feedback.contains("failed"),
        "first attempt feedback should record the worker failure, got: {first_feedback}"
    );
}

// ─── Test 6: capture_failure_propagates_as_error ───────────────────────────

#[tokio::test]
async fn capture_failure_propagates_as_error() {
    let repo = temp_repo();
    let quota = store();

    // Conductor only needs the plan — capture fails before evaluation.
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![ScriptedAdapter::ok_with_text(
            Provider::Claude,
            &plan_json(&[(1, "sabotage", "do it")]),
        )],
    ));

    // Worker destroys the git repo — capture_changes is an infrastructure
    // fault and must propagate as Err, not burn the rework budget.
    let worker = Arc::new(ScriptedAdapter {
        pre_script: "rm -rf .git".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "done")
    });

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let result = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await;

    assert!(
        result.is_err(),
        "capture_changes failure must propagate as Err"
    );
}

// ─── Test 7: two_subtasks_complete_in_order ────────────────────────────────

#[tokio::test]
async fn two_subtasks_complete_in_order() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: 2-subtask plan → accept → accept
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[
                    (1, "first file", "create one.txt"),
                    (2, "second file", "create two.txt"),
                ]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    // Worker: one file per subtask.
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo 'one' > one.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "created one.txt")
            },
            ScriptedAdapter {
                pre_script: "echo 'two' > two.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "created two.txt")
            },
        ],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "create two files",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1, 2]);
    assert!(outcome.halted.is_none());
    assert!(outcome.failed.is_none());
    assert!(repo.path().join("one.txt").exists());
    assert!(repo.path().join("two.txt").exists());

    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["id"], 1);
    assert_eq!(entries[1]["id"], 2);
}

#[tokio::test]
async fn budget_exceeded_stops_at_subtask_boundary() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![ScriptedAdapter::ok_with_text(
            Provider::Claude,
            &plan_json(&[
                (1, "first file", "create one.txt"),
                (2, "second file", "create two.txt"),
            ]),
        )],
    ));

    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo 'one' > one.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "created one.txt")
            },
            ScriptedAdapter {
                pre_script: "echo 'two' > two.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "created two.txt")
            },
        ],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker,
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: Some(Duration::from_millis(0)),
    };

    let outcome = run_conduct(
        "create two files",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    let failed = outcome.failed.expect("budget overrun should fail the run");
    assert!(
        failed.contains("budget"),
        "failure should mention budget: {failed}"
    );
    assert!(outcome.completed.is_empty());
    assert!(outcome.transcript["subtasks"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn budget_preserves_completed_subtasks_on_trip() {
    // The load-bearing guarantee: a budget that expires AFTER subtask 1 finishes
    // ships subtask 1 and skips subtask 2 (rather than discarding work). Made
    // deterministic by sleeping subtask 1's worker well past the budget.
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[
                    (1, "first", "create one.txt"),
                    (2, "second", "create two.txt"),
                ]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    // Subtask 1 sleeps 0.6s; budget is 200ms, so the boundary check before
    // subtask 2 trips (elapsed ~0.6s >= 0.2s) while subtask 1's own boundary
    // (a few ms in) does not. One worker step: subtask 2 must never run.
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![ScriptedAdapter {
            pre_script: "sleep 0.6; echo one > one.txt".into(),
            ..ScriptedAdapter::ok_with_text(Provider::Codex, "created one.txt")
        }],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker,
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: Some(Duration::from_millis(200)),
    };

    let outcome = run_conduct(
        "create two files",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1], "subtask 1 must be preserved");
    let failed = outcome.failed.expect("budget trip fails the run");
    assert!(
        failed.contains("budget") && failed.contains("shipped 1 of 2"),
        "got: {failed}"
    );
    assert!(
        repo.path().join("one.txt").exists(),
        "subtask 1's file persists"
    );
    assert!(!repo.path().join("two.txt").exists(), "subtask 2 never ran");
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], 1);
}

#[tokio::test]
async fn generous_budget_runs_all_subtasks() {
    // The Some-but-ample companion to the exceeded case: a budget that is never
    // hit must be behaviorally inert (all subtasks run, no failure).
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[
                    (1, "first", "create one.txt"),
                    (2, "second", "create two.txt"),
                ]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo one > one.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "created one.txt")
            },
            ScriptedAdapter {
                pre_script: "echo two > two.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "created two.txt")
            },
        ],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker,
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: Some(Duration::from_secs(3600)),
    };

    let outcome = run_conduct(
        "create two files",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1, 2], "an ample budget is inert");
    assert!(outcome.failed.is_none());
    assert!(repo.path().join("one.txt").exists() && repo.path().join("two.txt").exists());
    assert_eq!(outcome.transcript["subtasks"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn supervisor_failure_degrades_instead_of_killing_the_run() {
    // A transient supervisor failure (all rungs down) must NOT abort the run —
    // the supervisor is advisory. The run proceeds without its verdict.
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "create out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo hi > out.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    });
    // The supervisor's only rung always fails — its gate must degrade, not crash.
    let supervisor = Arc::new(ScriptedAdapter::failing(
        Provider::Gemini,
        "transient failure",
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker,
        )],
        supervisor: Some(solo_role_handle(Provider::Gemini, "g", supervisor)),
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "do it",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .expect("a supervisor failure must not error the whole run");

    assert_eq!(
        outcome.completed,
        vec![1],
        "run proceeds despite the supervisor being down"
    );
    assert!(outcome.halted.is_none() && outcome.failed.is_none());
    // The degrade is recorded as an "unavailable" supervisor entry, not hidden.
    let sup = &outcome.transcript["subtasks"].as_array().unwrap()[0]["supervisor"];
    assert!(
        serde_json::to_string(sup).unwrap().contains("unavailable"),
        "supervisor degrade should be recorded: {sup}"
    );
}

// ─── Test 8: fail_on_second_preserves_first ────────────────────────────────

#[tokio::test]
async fn fail_on_second_preserves_first() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: 2-subtask plan → accept (subtask 1) → fail (subtask 2)
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[
                    (1, "good part", "do part one"),
                    (2, "bad part", "do part two"),
                ]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &fail_json("wrong approach")),
        ],
    ));

    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo 'part one' > part1.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did part one")
            },
            ScriptedAdapter {
                pre_script: "echo 'part two' > part2.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did part two")
            },
        ],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "two part task",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    assert!(
        outcome
            .failed
            .as_deref()
            .unwrap_or("")
            .contains("wrong approach"),
        "failure reason should carry the conductor's feedback"
    );

    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    // Fail stops the subtask immediately — exactly one attempt on subtask 2.
    assert_eq!(entries[1]["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(entries[1]["attempts"][0]["decision"], "fail");
}

// ─── Test 9: critical_review_forces_rework ────────────────────────────────
// Reviewer returns critical findings on attempt 1, clean on attempt 2.
// Expected: completed == [1] after 2 attempts (conductor always accepts).

#[tokio::test]
async fn critical_review_forces_rework() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: plan → accept → accept (always willing to accept)
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "do thing", "do it")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    // Worker: writes a file both times
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo 'v1' > work.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "wrote v1")
            },
            ScriptedAdapter {
                pre_script: "echo 'v2' > work.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "wrote v2")
            },
        ],
    ));

    // Reviewer: first call → critical, second call → clean
    let reviewer = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &review_critical_json("work.txt", "sql injection"),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &review_clean_json()),
        ],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: Some(solo_role_handle(Provider::Claude, "model", reviewer)),
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.completed,
        vec![1],
        "should complete after 2 attempts"
    );
    assert!(outcome.failed.is_none());

    // 2 attempts total (first was reworked due to review)
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let attempts = entries[0]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "review gate should have forced a rework");
    // First attempt: conductor accepted but review blocked → "accept" decision recorded but review="critical"
    assert_eq!(attempts[0]["decision"], "accept");
    assert_eq!(attempts[0]["review"], "critical");
    // Second attempt: review clean → accepted
    assert_eq!(attempts[1]["decision"], "accept");
    assert_eq!(attempts[1]["review"], "clean");
}

#[tokio::test]
async fn reviewer_failure_degrades_to_accept() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "do thing", "do it")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo 'result' > out.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "done")
    });
    let reviewer = Arc::new(ScriptedAdapter::failing(
        Provider::Claude,
        "reviewer transient failure",
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker,
        )],
        supervisor: None,
        reviewer: Some(solo_role_handle(Provider::Claude, "model", reviewer)),
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .expect("reviewer failure must not error the whole run");

    assert_eq!(outcome.completed, vec![1]);
    assert!(outcome.failed.is_none());
    let attempts = outcome.transcript["subtasks"].as_array().unwrap()[0]["attempts"]
        .as_array()
        .unwrap();
    assert_eq!(attempts[0]["review"], "unavailable");
}

// ─── Test 10: arbiter_ships_on_exhaustion ────────────────────────────────
// Reviewer always returns critical (MAX_REWORKS exhausted at review gate).
// Arbiter ships → subtask completed; transcript records arbiter decision+reason.

#[tokio::test]
async fn arbiter_ships_on_exhaustion() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: plan → accept × 3 (always willing to accept — review keeps blocking)
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "do thing", "do it")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo 'result' > out.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "done")
    });

    // Reviewer: always critical (blocks all MAX_REWORKS+1 attempts)
    let reviewer = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &review_critical_json("out.txt", "persistent issue"),
    ));

    // Arbiter: ships
    let arbiter = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &arbiter_ship_json("findings are noise, ship it"),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: Some(solo_role_handle(Provider::Claude, "model", reviewer)),
        arbiter: Some(solo_role_handle(Provider::Claude, "model", arbiter)),
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.completed,
        vec![1],
        "arbiter ship should mark subtask as completed"
    );
    assert!(outcome.failed.is_none());

    // Transcript records arbiter decision+reason
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let arb = &entries[0]["arbiter"];
    assert_eq!(arb["decision"], "ship");
    assert!(
        arb["reason"].as_str().unwrap_or("").contains("noise"),
        "arbiter reason should be in transcript"
    );
}

// ─── Test 11: arbiter_fails_on_exhaustion ────────────────────────────────
// Reviewer always critical, arbiter returns fail → run stops (failed).

#[tokio::test]
async fn arbiter_fails_on_exhaustion() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "do thing", "do it")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "done"));

    let reviewer = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &review_critical_json("x.rs", "real blocker"),
    ));

    // Arbiter: fails
    let arbiter = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &arbiter_fail_json("findings are real, do not ship"),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: Some(solo_role_handle(Provider::Claude, "model", reviewer)),
        arbiter: Some(solo_role_handle(Provider::Claude, "model", arbiter)),
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert!(outcome.failed.is_some(), "arbiter fail should set failed");
    assert!(outcome.completed.is_empty());

    // Arbiter decision should be in transcript
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let arb = &entries[0]["arbiter"];
    assert_eq!(arb["decision"], "fail");
}

#[tokio::test]
async fn arbiter_failure_at_exhaustion_fails_closed() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "do thing", "do it")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "done"));

    let reviewer = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &review_critical_json("x.rs", "real blocker"),
    ));

    let arbiter = Arc::new(ScriptedAdapter::failing(
        Provider::Claude,
        "arbiter transient failure",
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: Some(solo_role_handle(Provider::Claude, "model", reviewer)),
        arbiter: Some(solo_role_handle(Provider::Claude, "model", arbiter)),
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .expect("arbiter failure must not error the whole run");

    assert!(outcome.failed.is_some());
    assert!(
        outcome.failed.as_deref().unwrap().contains("arbiter")
            && outcome.failed.as_deref().unwrap().contains("unavailable")
    );
    assert!(outcome.completed.is_empty());
}

// ─── Test 12: supervisor_ok_does_not_interfere ───────────────────────────
// Supervisor returns ok → happy path completes normally; transcript has "ok".

#[tokio::test]
async fn supervisor_ok_does_not_interfere() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "create file", "create ok.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo 'hello' > ok.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "created ok.txt")
    });

    let supervisor = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &supervisor_ok_json(),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: Some(solo_role_handle(Provider::Claude, "model", supervisor)),
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "create a file",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    assert!(outcome.halted.is_none());
    assert!(outcome.failed.is_none());
    assert!(repo.path().join("ok.txt").exists());

    // Supervisor entry should show status "ok"
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    let sup_entries = entries[0]["supervisor"].as_array().unwrap();
    assert!(!sup_entries.is_empty());
    assert_eq!(sup_entries[0]["status"], "ok");
}

// ─── Test 13: supervisor_concern_threads_note_into_evaluation ─────────────
// Supervisor returns a concern with a specific note. The conductor's evaluation
// prompt must contain that note text (verified via RecordingAdapter).
//
// RecordingAdapter wraps ScriptedAdapter directly, so we build a minimal
// `RecordingSequenced` helper (local struct) that sequences two RecordingAdapters
// of Arc<dyn Adapter> and records each build_command call into a shared log.

#[tokio::test]
async fn supervisor_concern_threads_note_into_evaluation() {
    use consilium::adapters::claude::ClaudeAdapter;

    // Sequences a Vec<Arc<dyn Adapter>> with clamping, recording each call prompt
    // into a shared log — lets us assert what prompt reached the conductor.
    struct RecordingSequenced {
        provider: Provider,
        steps: Vec<Arc<dyn consilium::adapters::Adapter>>,
        cursor: std::sync::atomic::AtomicUsize,
        log: Arc<Mutex<Vec<(String, bool, bool)>>>,
    }
    impl consilium::adapters::Adapter for RecordingSequenced {
        fn provider(&self) -> consilium::event::Provider {
            self.provider
        }
        fn cli_binary(&self) -> &'static str {
            "sh"
        }
        fn build_command(&self, req: &consilium::adapters::RunRequest) -> tokio::process::Command {
            {
                let mut guard = self.log.lock().unwrap();
                guard.push((req.prompt.clone(), req.advisory, req.write));
            }
            let i = self
                .cursor
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .min(self.steps.len().saturating_sub(1));
            self.steps[i].build_command(req)
        }
        fn parse_line(&self, line: &str) -> Vec<consilium::event::AgentEvent> {
            ClaudeAdapter.parse_line(line)
        }
    }

    let repo = temp_repo();
    let quota = store();

    let concern_note = "scope is drifting into unrelated modules";
    let shared_log: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    // Conductor: step 0 = plan, step 1 = accept.
    // Both steps are plain ScriptedAdapters; the RecordingSequenced records
    // the prompt before delegating, giving us a post-run log.
    let plan_adapter = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &plan_json(&[(1, "create file", "create concern.txt")]),
    ));
    let accept_adapter = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &accept_json(),
    ));

    let conductor = Arc::new(RecordingSequenced {
        provider: Provider::Claude,
        steps: vec![plan_adapter, accept_adapter],
        cursor: std::sync::atomic::AtomicUsize::new(0),
        log: shared_log.clone(),
    });

    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo 'content' > concern.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "created concern.txt")
    });

    let supervisor = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &supervisor_concern_json(concern_note),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: Some(solo_role_handle(Provider::Claude, "model", supervisor)),
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "create a file",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    assert!(outcome.halted.is_none());

    // calls[0] = decompose prompt (advisory=true, write=false)
    // calls[1] = evaluation prompt (advisory=true, write=false) — must contain the note
    let calls = shared_log.lock().unwrap();
    assert!(
        calls.len() >= 2,
        "conductor should have been called at least twice; got {} calls",
        calls.len()
    );
    let eval_prompt = &calls[1].0;
    assert!(
        eval_prompt.contains(concern_note),
        "evaluation prompt must contain the supervisor note; got:\n{eval_prompt}"
    );
}

// ─── Test: conductor decompose infra-failure surfaces, not "no plan" ───────

#[tokio::test]
async fn decompose_session_failure_surfaces_real_error() {
    let repo = temp_repo();
    let quota = store();

    // Conductor session itself fails (e.g. model 404) — this must NOT be
    // reported as "conductor produced no plan".
    let conductor = Arc::new(ScriptedAdapter::failing(
        Provider::Claude,
        "model claude-fable-5 not accessible",
    ));

    let worker = Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "unused"));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let err = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    // With failover, a single-rung conductor whose only rung fails produces:
    // "conductor: all N model rungs failed: ..."
    // The important invariant: NOT "conductor produced no plan" (the model failure
    // is a real infrastructure error, not a parse failure).
    assert!(
        msg.contains("conductor") && msg.contains("failed"),
        "expected a conductor-failure message, got: {msg}"
    );
    assert!(
        !msg.contains("produced no plan"),
        "infra failure must not masquerade as 'no plan', got: {msg}"
    );
}

// ─── conduct_worker_falls_back: worker's primary model is dead, ladder recovers ─

#[tokio::test]
async fn conduct_worker_falls_back() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: plan (1 subtask) then accept.
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "create file", "create out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    // Worker ladder: rung 0 = a dead model (ModelUnavailable), rung 1 = writes the file.
    let dead_rung = Rung {
        candidate: ModelCandidate {
            provider: Provider::Claude,
            model: "claude-fable-5".into(),
        },
        adapter: Arc::new(ScriptedAdapter::failing(
            Provider::Claude,
            "There's an issue with the selected model (claude-fable-5). It may not exist or you may not have access to it.",
        )),
    };
    let live_rung = Rung {
        candidate: ModelCandidate {
            provider: Provider::Claude,
            model: "claude-opus-4-8".into(),
        },
        adapter: Arc::new(ScriptedAdapter {
            pre_script: "echo 'hello' > out.txt".into(),
            ..ScriptedAdapter::ok_with_text(Provider::Claude, "created out.txt")
        }),
    };
    let worker = CouncilMember {
        label: "claude-worker".into(),
        ladder: vec![dead_rung, live_rung],
    };

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "claude-opus-4-8", conductor),
        workers: vec![worker],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "create a file",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    assert!(
        repo.path().join("out.txt").exists(),
        "fallback model should have created out.txt"
    );
    let fallbacks = outcome.transcript["fallbacks"].as_array().unwrap();
    assert!(
        !fallbacks.is_empty(),
        "a worker model-unavailable demotion should be recorded in transcript fallbacks"
    );
}

// ─── Test 16: failing_tests_force_rework_even_if_conductor_would_accept ──────
// Grounding rule keystone: when verify ran and failed, Accept is overridden to
// Rework regardless of the conductor's text opinion.
// Attempt 1: worker writes "bad" to out.txt → verify (grep -q good out.txt) fails
//            → conductor would accept but grounding rule forces Rework.
// Attempt 2: worker writes "good" to out.txt → verify passes → accept stands.
// Expected: completed == [1], attempts[0].decision=="rework", attempts[0].verify=="failed",
//           attempts[1].decision=="accept", attempts[1].verify=="passed".

#[tokio::test]
async fn failing_tests_force_rework_even_if_conductor_would_accept() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: plan, then accept twice (it WOULD accept both attempts)
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "write out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    // Worker attempt 1: writes "bad" to out.txt; attempt 2: writes "good" to out.txt.
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo bad > out.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
            },
            ScriptedAdapter {
                pre_script: "echo good > out.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "fixed it")
            },
        ],
    ));

    // Verify: a test command that passes only when out.txt contains "good".
    let verify = VerifyConfig {
        test: Some("grep -q good out.txt".into()),
        build: None,
        lint: None,
        timeout_secs: None,
    };

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: Some(verify),
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    let attempts = outcome.transcript["subtasks"][0]["attempts"]
        .as_array()
        .unwrap();
    assert_eq!(attempts.len(), 2);
    // Attempt 1: conductor would accept but verify failed → grounding override → rework
    assert_eq!(attempts[0]["decision"], "rework");
    assert_eq!(attempts[0]["verify"], "failed");
    // Attempt 2: verify passed → conductor's accept stands
    assert_eq!(attempts[1]["decision"], "accept");
    assert_eq!(attempts[1]["verify"], "passed");
}

// ─── Test 17: no_verifier_is_recorded_as_unverified ──────────────────────────
// When no verify config is given and the temp repo has no ecosystem markers
// (no Cargo.toml, package.json, etc.), verify does not run and the transcript
// records "not_run" for the attempt.

#[tokio::test]
async fn no_verifier_is_recorded_as_unverified() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "write out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo hi > out.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    });

    // No verify config; temp repo has no Cargo.toml etc. — auto-detection yields nothing.
    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    let attempts = outcome.transcript["subtasks"][0]["attempts"]
        .as_array()
        .unwrap();
    assert_eq!(attempts[0]["verify"], "not_run");
}

// ─── ConductorMemory (P0 #2) ────────────────────────────────────────────────

/// A worker that (re)writes out.txt on every attempt, recording success.
fn writing_worker() -> Arc<ScriptedAdapter> {
    Arc::new(ScriptedAdapter {
        pre_script: "echo content > out.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    })
}

// Test: the conductor's judgment prompt for attempt N carries the prior
// attempts' history (its own earlier feedback), so it stops repeating itself.
#[tokio::test]
async fn attempt_history_threads_prior_feedback() {
    let repo = temp_repo();
    let quota = store();
    let log: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    // Conductor calls: 0 = decompose, 1 = judge attempt 0 -> rework, 2 = judge attempt 1 -> accept.
    let conductor = Arc::new(RecordingSequenced::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "write out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &rework_json("ADD_DOCS_MARKER")),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
        log.clone(),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", writing_worker())],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(), // enabled
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, vec![1]);

    let calls = log.lock().unwrap();
    // Exact count: decompose + judge(attempt 0) + judge(attempt 1). Asserting the
    // exact count (not >=) catches an over-call regression the clamping would hide.
    assert_eq!(
        calls.len(),
        3,
        "expected exactly decompose + 2 judgments, got {}",
        calls.len()
    );
    // Attempt 0's judgment: no prior attempts -> no history block.
    assert!(
        !calls[1].0.contains("<attempt_history>"),
        "first attempt judgment must have no attempt_history block"
    );
    // Attempt 1's judgment: carries the prior round's exact history line.
    assert!(
        calls[2].0.contains("<attempt_history>"),
        "second attempt judgment must carry an attempt_history block"
    );
    assert!(
        calls[2]
            .0
            .contains("attempt 0: rework (verify: not_run) — ADD_DOCS_MARKER"),
        "second attempt judgment must echo the prior round's exact history line; got:\n{}",
        calls[2].0
    );
}

// Test: subtask N's conductor prompt carries a plan ledger of the prior
// finished subtasks; subtask 1's prompt does not.
#[tokio::test]
async fn plan_ledger_threads_prior_subtasks() {
    let repo = temp_repo();
    let quota = store();
    let log: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    // Conductor calls: 0 = decompose, 1 = judge sub1 accept, 2 = judge sub2 accept.
    let conductor = Arc::new(RecordingSequenced::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "alpha_subtask", "do a"), (2, "beta_subtask", "do b")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
        log.clone(),
    ));

    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo x >> f.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    });

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(), // enabled
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, vec![1, 2]);

    let calls = log.lock().unwrap();
    assert_eq!(
        calls.len(),
        3,
        "expected exactly decompose + 2 judgments, got {}",
        calls.len()
    );
    // Subtask 1's judgment: no prior subtasks -> no ledger.
    assert!(
        !calls[1].0.contains("<plan_ledger>"),
        "first subtask judgment must have no plan_ledger block"
    );
    // Subtask 2's judgment: ledger lists subtask 1 as completed.
    assert!(
        calls[2].0.contains("<plan_ledger>"),
        "second subtask judgment must carry a plan_ledger block"
    );
    assert!(
        calls[2].0.contains("alpha_subtask") && calls[2].0.contains("completed"),
        "ledger must show subtask 1 completed; got:\n{}",
        calls[2].0
    );
}

// Keystone: a grounding-overridden accept must appear in the attempt history as
// `rework`/`failed`, never `accept`.
#[tokio::test]
async fn grounding_override_recorded_in_history() {
    let repo = temp_repo();
    let quota = store();
    let log: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    // Conductor always says accept; verify forces the first attempt to rework.
    let conductor = Arc::new(RecordingSequenced::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "write out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
        log.clone(),
    ));

    // Attempt 0 writes "bad" (verify fails); attempt 1 writes "good" (passes).
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo bad > out.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
            },
            ScriptedAdapter {
                pre_script: "echo good > out.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "fixed it")
            },
        ],
    ));

    let verify = VerifyConfig {
        test: Some("grep -q good out.txt".into()),
        build: None,
        lint: None,
        timeout_secs: None,
    };

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: Some(verify),
        memory: Default::default(), // enabled
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, vec![1]);

    // Ledger layer: the recorded decision for attempt 0 is the post-override rework.
    let attempts = outcome.transcript["subtasks"][0]["attempts"]
        .as_array()
        .unwrap();
    assert_eq!(attempts[0]["decision"], "rework");
    assert_eq!(attempts[0]["verify"], "failed");

    // Prompt layer: attempt 1's history shows attempt 0 as rework/failed, not accept.
    let calls = log.lock().unwrap();
    assert_eq!(
        calls.len(),
        3,
        "expected exactly decompose + 2 judgments, got {}",
        calls.len()
    );
    let judged = &calls[2].0;
    assert!(
        judged.contains("attempt 0: rework (verify: failed)"),
        "attempt history must record the post-override rework; got:\n{judged}"
    );
    assert!(
        !judged.contains("attempt 0: accept"),
        "the grounding-overridden round must never show as accept"
    );
}

// With memory disabled, prompts are byte-identical to the pre-memory behavior:
// no ledger / history blocks ever appear.
#[tokio::test]
async fn memory_disabled_is_byte_identical() {
    let repo = temp_repo();
    let quota = store();
    let log: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    let conductor = Arc::new(RecordingSequenced::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "write out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &rework_json("more please")),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
        log.clone(),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", writing_worker())],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: ConductorMemoryConfig {
            enabled: false,
            ledger_char_cap: 1500,
            attempt_history_char_cap: 800,
        },
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, vec![1]);

    let calls = log.lock().unwrap();
    for (prompt, _, _) in calls.iter() {
        assert!(
            !prompt.contains("<plan_ledger>"),
            "memory off -> no plan_ledger"
        );
        assert!(
            !prompt.contains("<attempt_history>"),
            "memory off -> no attempt_history"
        );
    }
}

// The supervisor — whose job is to catch repeated failures and scope drift —
// must receive the cross-subtask plan ledger.
#[tokio::test]
async fn supervisor_receives_plan_ledger() {
    let repo = temp_repo();
    let quota = store();
    let sup_log: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "alpha_subtask", "do a"), (2, "beta_subtask", "do b")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    let supervisor = Arc::new(RecordingSequenced::new(
        Provider::Gemini,
        vec![
            ScriptedAdapter::ok_with_text(Provider::Gemini, &supervisor_ok_json()),
            ScriptedAdapter::ok_with_text(Provider::Gemini, &supervisor_ok_json()),
        ],
        sup_log.clone(),
    ));
    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo x >> f.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    });

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: Some(solo_role_handle(Provider::Gemini, "m", supervisor)),
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, vec![1, 2]);

    let calls = sup_log.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "supervisor runs once per subtask, got {}",
        calls.len()
    );
    assert!(
        !calls[0].0.contains("<plan_ledger>"),
        "subtask 1 supervisor: no prior subtasks → no ledger"
    );
    assert!(
        calls[1].0.contains("<plan_ledger>") && calls[1].0.contains("alpha_subtask"),
        "subtask 2 supervisor must carry the plan ledger; got:\n{}",
        calls[1].0
    );
}

// The arbiter (final appeal at rework exhaustion) must receive BOTH memory
// blocks: the cross-subtask ledger and this subtask's attempt history.
#[tokio::test]
async fn arbiter_receives_memory_blocks() {
    let repo = temp_repo();
    let quota = store();
    let arb_log: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    // Conductor accepts every time (clamps to the last step).
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "alpha_subtask", "do a"), (2, "beta_subtask", "do b")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    // Reviewer: clean for subtask 1, then critical for all of subtask 2's attempts.
    let reviewer = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter::ok_with_text(Provider::Codex, &review_clean_json()),
            ScriptedAdapter::ok_with_text(Provider::Codex, &review_critical_json("f.txt", "bug")),
            ScriptedAdapter::ok_with_text(Provider::Codex, &review_critical_json("f.txt", "bug")),
            ScriptedAdapter::ok_with_text(Provider::Codex, &review_critical_json("f.txt", "bug")),
        ],
    ));
    let arbiter = Arc::new(RecordingSequenced::new(
        Provider::Claude,
        vec![ScriptedAdapter::ok_with_text(
            Provider::Claude,
            &arbiter_ship_json("acceptable"),
        )],
        arb_log.clone(),
    ));
    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo x >> f.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    });

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: Some(solo_role_handle(Provider::Codex, "m", reviewer)),
        arbiter: Some(solo_role_handle(Provider::Claude, "m", arbiter)),
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, vec![1, 2]);

    let calls = arb_log.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "arbiter fires once at exhaustion, got {}",
        calls.len()
    );
    let p = &calls[0].0;
    assert!(
        p.contains("<plan_ledger>") && p.contains("alpha_subtask"),
        "arbiter must carry the cross-subtask ledger; got:\n{p}"
    );
    assert!(
        p.contains("<attempt_history>"),
        "arbiter must carry the subtask's attempt history; got:\n{p}"
    );
}

// History accumulates across multiple rework rounds: the third judgment sees
// both earlier feedback strings, not just the latest.
#[tokio::test]
async fn multi_rework_history_accumulates() {
    let repo = temp_repo();
    let quota = store();
    let log: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    // decompose, rework MSG_ONE, rework MSG_TWO, accept (attempts 0,1,2).
    let conductor = Arc::new(RecordingSequenced::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "write out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &rework_json("MSG_ONE")),
            ScriptedAdapter::ok_with_text(Provider::Claude, &rework_json("MSG_TWO")),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
        log.clone(),
    ));

    // Distinct write per attempt so the worker doesn't trip the P1.5 stall
    // breaker — this test is about the conductor's feedback history accumulating.
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo one > out.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
            },
            ScriptedAdapter {
                pre_script: "echo two > out.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
            },
            ScriptedAdapter {
                pre_script: "echo three > out.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
            },
        ],
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, vec![1]);

    let calls = log.lock().unwrap();
    assert_eq!(
        calls.len(),
        4,
        "expected decompose + 3 judgments, got {}",
        calls.len()
    );
    // The third judgment (attempt 2) must see BOTH prior rounds' feedback.
    let third = &calls[3].0;
    assert!(
        third.contains("MSG_ONE") && third.contains("MSG_TWO"),
        "accumulated history must contain both prior feedbacks; got:\n{third}"
    );
}

// ─── P0 #3: worker blackboard ───────────────────────────────────────────────

// Worker N's INITIAL prompt inherits a mechanical roster of prior finished
// subtasks + the files this run already touched.
#[tokio::test]
async fn blackboard_threads_prior_subtasks_to_worker() {
    let repo = temp_repo();
    let quota = store();
    let wlog: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[
                    (1, "build_math", "create m.rs"),
                    (2, "build_text", "create t.rs"),
                ]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    // The worker records its prompts and writes a distinct file per subtask.
    let worker = Arc::new(RecordingSequenced::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo m > m.rs".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did sub1")
            },
            ScriptedAdapter {
                pre_script: "echo t > t.rs".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did sub2")
            },
        ],
        wlog.clone(),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, vec![1, 2]);

    let calls = wlog.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "worker runs once per subtask, got {}",
        calls.len()
    );
    // Subtask 1: no prior work → bare prompt, no blackboard.
    assert!(
        !calls[0].0.contains("<prior_work>"),
        "first worker must have no prior_work block"
    );
    // Subtask 2: inherits the roster + the file subtask 1 created.
    let w2 = &calls[1].0;
    assert!(
        w2.contains("<prior_work>"),
        "second worker must get a prior_work block; got:\n{w2}"
    );
    assert!(
        w2.contains("build_math") && w2.contains("completed"),
        "roster must show subtask 1 completed; got:\n{w2}"
    );
    assert!(
        w2.contains("m.rs"),
        "blackboard must list the file subtask 1 created; got:\n{w2}"
    );
    // The files line lists ONLY m.rs — subtask 2's own t.rs isn't written yet, so
    // the this-run filter excludes it. (The worker's own subtask prompt
    // "create t.rs" naturally mentions t.rs, so scope the check to the files line.)
    assert!(
        w2.contains("files modified this run: m.rs\n</prior_work>"),
        "files line must list only m.rs, not subtask 2's own t.rs; got:\n{w2}"
    );
    assert!(
        !w2.contains("verify:"),
        "blackboard is mechanical — no verify digest may leak to the worker"
    );
}

// Roster + file list accumulate across 3+ subtasks: subtask 3's worker sees
// both prior subtasks and both files they created.
#[tokio::test]
async fn blackboard_accumulates_across_three_subtasks() {
    let repo = temp_repo();
    let quota = store();
    let wlog: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[
                    (1, "build_a", "create a.rs"),
                    (2, "build_b", "create b.rs"),
                    (3, "build_c", "create c.rs"),
                ]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    let worker = Arc::new(RecordingSequenced::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo a > a.rs".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did a")
            },
            ScriptedAdapter {
                pre_script: "echo b > b.rs".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did b")
            },
            ScriptedAdapter {
                pre_script: "echo c > c.rs".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did c")
            },
        ],
        wlog.clone(),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, vec![1, 2, 3]);

    let calls = wlog.lock().unwrap();
    assert_eq!(
        calls.len(),
        3,
        "worker runs once per subtask, got {}",
        calls.len()
    );
    let w3 = &calls[2].0;
    // Subtask 3's worker sees both prior subtasks and both prior files.
    assert!(
        w3.contains("build_a") && w3.contains("build_b"),
        "got:\n{w3}"
    );
    // Files line lists only the prior subtasks' files (sorted), not subtask 3's
    // own c.rs (not written yet). Scope to the files line — the subtask prompt
    // "create c.rs" itself mentions c.rs.
    assert!(
        w3.contains("files modified this run: a.rs, b.rs\n</prior_work>"),
        "files line must list only prior a.rs, b.rs (not subtask 3's own c.rs); got:\n{w3}"
    );
}

// With memory off, the worker's prompt is byte-identical to the raw subtask
// prompt — no blackboard ever appears.
#[tokio::test]
async fn blackboard_disabled_is_byte_identical() {
    let repo = temp_repo();
    let quota = store();
    let wlog: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[
                    (1, "build_math", "create m.rs"),
                    (2, "build_text", "create t.rs"),
                ]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    let worker = Arc::new(RecordingSequenced::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo m > m.rs".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did sub1")
            },
            ScriptedAdapter {
                pre_script: "echo t > t.rs".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did sub2")
            },
        ],
        wlog.clone(),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: ConductorMemoryConfig {
            enabled: false,
            ledger_char_cap: 1500,
            attempt_history_char_cap: 800,
        },
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, vec![1, 2]);

    let calls = wlog.lock().unwrap();
    assert_eq!(calls.len(), 2);
    for (p, _, _) in calls.iter() {
        assert!(!p.contains("<prior_work>"), "memory off → no blackboard");
    }
    // The worker prompt carries the raw subtask + scope-discipline preamble (and,
    // with memory off, no blackboard).
    assert!(calls[0].0.contains("create m.rs"));
    assert!(calls[0].0.contains("Scope discipline"));
}

// ─── M3c: cross-family review ────────────────────────────────────────────────

// With cross_family_review on, a Codex-worker diff is reviewed by a DIFFERENT
// family. The same-family (Codex) reviewer is wired to FAIL, so a clean run
// proves the Gemini worker fronted the review (Finding 7). Marker: "applied".
#[tokio::test]
async fn cross_family_review_routes_to_a_different_family() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "write out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    // Codex worker (chosen for the subtask: first in order, ties at 0 quota) writes
    // the file; the Gemini worker doubles as the cross-family reviewer (clean).
    let codex_worker = Arc::new(ScriptedAdapter {
        pre_script: "echo hi > out.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    });
    let gemini = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Gemini,
        &review_clean_json(),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![
            solo_worker("codex", Provider::Codex, "gpt", codex_worker),
            solo_worker("gemini", Provider::Gemini, "g", gemini),
        ],
        supervisor: None,
        // Same-family reviewer is set to FAIL — it must NOT be the one used.
        reviewer: Some(solo_role_handle(
            Provider::Codex,
            "rev",
            Arc::new(ScriptedAdapter::failing(
                Provider::Codex,
                "same-family reviewer must not be reached",
            )),
        )),
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: true,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    let att = &outcome.transcript["subtasks"][0]["attempts"][0];
    assert_eq!(
        att["cross_family"], "applied",
        "review should be routed cross-family; attempt: {att}"
    );
    assert_eq!(att["review"], "clean", "the Gemini reviewer returned clean");
}

// Single-family deployment: cross-family degrades to the same-family reviewer
// (fail-open) and marks it, never blocking the review.
#[tokio::test]
async fn cross_family_degrades_same_family_when_no_other_family() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "write out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo hi > out.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    });
    // Only Codex everywhere; reviewer (Codex) returns clean.
    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "gpt", worker)],
        supervisor: None,
        reviewer: Some(solo_role_handle(
            Provider::Codex,
            "rev",
            Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Codex,
                &review_clean_json(),
            )),
        )),
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: true,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    let att = &outcome.transcript["subtasks"][0]["attempts"][0];
    assert_eq!(
        att["cross_family"], "degraded_same_family",
        "attempt: {att}"
    );
    assert_eq!(att["review"], "clean");
}

// The arbiter gate also routes cross-family: a Codex worker, a Gemini reviewer
// that keeps flagging critical (→ reworks exhaust), and a Gemini arbiter that
// ships. A completed run proves the arbiter ran under the flag and shipped.
#[tokio::test]
async fn cross_family_arbiter_runs_and_ships() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "write out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo hi > out.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    });
    let reviewer = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Gemini,
        &review_critical_json("out.txt", "nit"),
    ));
    let arbiter = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Gemini,
        &arbiter_ship_json("acceptable"),
    ));

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "gpt", worker)],
        supervisor: None,
        reviewer: Some(solo_role_handle(Provider::Gemini, "rev", reviewer)),
        arbiter: Some(solo_role_handle(Provider::Gemini, "arb", arbiter)),
        verify: None,
        memory: Default::default(),
        cross_family_review: true,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.completed,
        vec![1],
        "arbiter should ship the subtask"
    );
    let st = &outcome.transcript["subtasks"][0];
    assert_eq!(st["arbiter"]["decision"], "ship", "subtask: {st}");
    assert_eq!(st["attempts"][0]["cross_family"], "applied");
}

// ─── Failure isolation: an independent subtask runs despite an earlier failure ──
#[tokio::test]
async fn independent_subtask_runs_despite_an_earlier_failure() {
    let repo = temp_repo();
    let quota = store();
    // Subtask 1 (no deps) FAILS at conductor eval; subtask 2 (no deps) is
    // independent → must still run+complete. Both wave 0 (slice order).
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json_with_deps(&[
                    (1, "doomed", "do part one", &[]),
                    (2, "independent", "do part two", &[]),
                ]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &fail_json("subtask 1 is wrong")),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter {
                pre_script: "echo one > one.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did part one")
            },
            ScriptedAdapter {
                pre_script: "echo two > two.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "did part two")
            },
        ],
    ));
    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker,
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };
    let outcome = run_conduct(
        "two independent parts",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert_eq!(
        outcome.completed,
        vec![2],
        "independent subtask 2 runs despite subtask 1's failure"
    );
    assert!(
        outcome.failed.as_deref().unwrap_or("").contains("wrong"),
        "the run still reports subtask 1's failure"
    );
    assert!(
        repo.path().join("two.txt").exists(),
        "subtask 2's work landed"
    );
    assert!(
        outcome.transcript["skipped"].as_array().unwrap().is_empty(),
        "nothing is skipped (subtask 2 has no unmet deps)"
    );
}

// ─── skip-failed-dependency: a dependent is skipped when its prereq fails ──────
#[tokio::test]
async fn dependent_subtask_is_skipped_when_prerequisite_fails() {
    let repo = temp_repo();
    let quota = store();
    // Subtask 1 (no deps) FAILS; subtask 2 depends_on [1] → SKIPPED (never run).
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json_with_deps(&[
                    (1, "prereq", "do the prerequisite", &[]),
                    (2, "dependent", "do the dependent work", &[1]),
                ]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &fail_json("prereq failed")),
            // No eval for subtask 2 — it is skipped before any worker/eval runs.
        ],
    ));
    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo one > one.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did prereq")
    });
    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "model", conductor),
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker,
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };
    let outcome = run_conduct(
        "prereq then dependent",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();
    assert!(
        outcome.completed.is_empty(),
        "prereq failed → nothing completed"
    );
    let skipped = outcome.transcript["skipped"].as_array().unwrap();
    assert!(
        skipped.iter().any(|v| v.as_u64() == Some(2)),
        "the dependent subtask is recorded skipped: {skipped:?}"
    );
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    let s2 = entries
        .iter()
        .find(|e| e["id"] == 2)
        .expect("subtask 2 entry exists");
    assert_eq!(s2["status"], "skipped");
    assert!(
        s2["attempts"].as_array().unwrap().is_empty(),
        "a skipped subtask records no attempts"
    );
}

// With the flag OFF (default), no cross_family marker is emitted — pinning the
// byte-identity claim explicitly.
#[tokio::test]
async fn cross_family_off_emits_no_marker() {
    let repo = temp_repo();
    let quota = store();

    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "x", "write out.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    let worker = Arc::new(ScriptedAdapter {
        pre_script: "echo hi > out.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    });
    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "gpt", worker)],
        supervisor: None,
        reviewer: Some(solo_role_handle(
            Provider::Codex,
            "rev",
            Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Codex,
                &review_clean_json(),
            )),
        )),
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    };

    let outcome = run_conduct(
        "t",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        &health(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    let att = &outcome.transcript["subtasks"][0]["attempts"][0];
    assert!(
        att.get("cross_family").is_none(),
        "flag off → no cross_family marker; attempt: {att}"
    );
}
