mod common;

#[allow(unused_imports)]
use common::{RecordingAdapter, ScriptedAdapter, SequencedAdapter};
use consilium::config::ModelCandidate;
use consilium::event::Provider;
use consilium::orchestrator::conduct::{run_conduct, ConductDeps, ConductOutcome, RoleHandle};
use consilium::orchestrator::council::CouncilMember;
use consilium::orchestrator::resilience::Rung;
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
/// Task 6 will replace these with real multi-rung ladders.
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
    };

    let outcome: ConductOutcome = run_conduct(
        "create a file",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
    };

    let outcome = run_conduct(
        "write content",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: Some(RoleHandle {
            adapter: supervisor,
            model: None,
        }),
        reviewer: None,
        arbiter: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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

// ─── Test 5: worker_failure_counts_as_attempt ──────────────────────────────

#[tokio::test]
async fn worker_failure_counts_as_attempt() {
    let repo = temp_repo();
    let quota = store();

    // Conductor: plan → accept (after worker failure + retry)
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

    // Worker: step 0 fails, step 1 succeeds and creates the file
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter::failing(Provider::Codex, "worker error: quota exceeded"),
            ScriptedAdapter {
                pre_script: "echo 'recovered' > recovered.txt".into(),
                ..ScriptedAdapter::ok_with_text(Provider::Codex, "recovered successfully")
            },
        ],
    ));

    let deps = ConductDeps {
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
    )
    .await
    .unwrap();

    assert_eq!(outcome.completed, vec![1]);
    assert!(outcome.failed.is_none());

    // Transcript: 2 attempts; first attempt feedback contains the worker error
    let entries = outcome.transcript["subtasks"].as_array().unwrap();
    let attempts = entries[0]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    let first_feedback = attempts[0]["feedback"].as_str().unwrap_or("");
    assert!(
        first_feedback.contains("quota exceeded") || first_feedback.contains("worker error"),
        "first attempt feedback should contain the worker error, got: {first_feedback}"
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
    };

    let result = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
    };

    let outcome = run_conduct(
        "create two files",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
    };

    let outcome = run_conduct(
        "two part task",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: Some(RoleHandle {
            adapter: reviewer,
            model: None,
        }),
        arbiter: None,
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: Some(RoleHandle {
            adapter: reviewer,
            model: None,
        }),
        arbiter: Some(RoleHandle {
            adapter: arbiter,
            model: None,
        }),
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: Some(RoleHandle {
            adapter: reviewer,
            model: None,
        }),
        arbiter: Some(RoleHandle {
            adapter: arbiter,
            model: None,
        }),
    };

    let outcome = run_conduct(
        "do thing",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: Some(RoleHandle {
            adapter: supervisor,
            model: None,
        }),
        reviewer: None,
        arbiter: None,
    };

    let outcome = run_conduct(
        "create a file",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: Some(RoleHandle {
            adapter: supervisor,
            model: None,
        }),
        reviewer: None,
        arbiter: None,
    };

    let outcome = run_conduct(
        "create a file",
        "",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
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
        conductor: RoleHandle {
            adapter: conductor,
            model: None,
        },
        workers: vec![solo_worker(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            worker.clone(),
        )],
        supervisor: None,
        reviewer: None,
        arbiter: None,
    };

    let err = run_conduct("t", "", deps, &quota, repo.path().to_path_buf(), TIMEOUT)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("conductor decompose failed"),
        "expected decompose-failure message, got: {msg}"
    );
}
