mod common;

use common::{ScriptedAdapter, SequencedAdapter};
use consilium::event::Provider;
use consilium::orchestrator::conduct::{run_conduct, ConductDeps, ConductOutcome, RoleHandle};
use consilium::orchestrator::council::CouncilMember;
use consilium::quota::QuotaStore;
use std::sync::Arc;
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

fn store() -> QuotaStore {
    QuotaStore::open_in_memory().unwrap()
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
        workers: vec![CouncilMember {
            label: "codex-worker".into(),
            adapter: worker,
            model: None,
        }],
        supervisor: None,
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
        workers: vec![CouncilMember {
            label: "codex-worker".into(),
            adapter: worker,
            model: None,
        }],
        supervisor: None,
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
        workers: vec![CouncilMember {
            label: "codex-worker".into(),
            adapter: worker,
            model: None,
        }],
        supervisor: None,
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
        workers: vec![CouncilMember {
            label: "codex-worker".into(),
            adapter: worker,
            model: None,
        }],
        supervisor: Some(RoleHandle {
            adapter: supervisor,
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
        workers: vec![CouncilMember {
            label: "codex-worker".into(),
            adapter: worker,
            model: None,
        }],
        supervisor: None,
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
        workers: vec![CouncilMember {
            label: "codex-worker".into(),
            adapter: worker,
            model: None,
        }],
        supervisor: None,
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
        workers: vec![CouncilMember {
            label: "codex-worker".into(),
            adapter: worker,
            model: None,
        }],
        supervisor: None,
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
        workers: vec![CouncilMember {
            label: "codex-worker".into(),
            adapter: worker,
            model: None,
        }],
        supervisor: None,
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
