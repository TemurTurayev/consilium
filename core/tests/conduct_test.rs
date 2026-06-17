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
    };

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: Some(verify),
        memory: Default::default(),
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
    };

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: Some(verify),
        memory: Default::default(), // enabled
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

    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", writing_worker())],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
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
    assert!(
        !w2.contains("verify:"),
        "blackboard is mechanical — no verify digest may leak to the worker"
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
    // Byte-identity: subtask 1's worker prompt is exactly the raw subtask prompt.
    assert_eq!(calls[0].0, "create m.rs");
}
