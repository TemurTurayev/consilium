mod common;

use common::{ScriptedAdapter, SequencedAdapter};
use consilium::event::Provider;
use consilium::orchestrator::auto::{run_auto, AutoDeps};
use consilium::orchestrator::conduct::{ConductDeps, RoleHandle};
use consilium::orchestrator::council::CouncilMember;
use consilium::quota::QuotaStore;
use std::time::Duration;

// ─── helpers (copied from conduct_test.rs) ───────────────────────────────────

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

fn council_ok_json(synthesis: &str) -> String {
    // Chairman synthesis response: plain text (no JSON needed — council uses it as-is).
    synthesis.to_string()
}

fn store() -> QuotaStore {
    QuotaStore::open_in_memory().unwrap()
}

const TIMEOUT: Duration = Duration::from_secs(30);

// ─── Test 1: trivial_skips_council ───────────────────────────────────────────
//
// Conductor sequence: [triage "trivial", plan(1 subtask), accept].
// Council members are `ScriptedAdapter::failing` — if council ran, run_council
// would bail (no member produced an answer) and run_auto would return Err.
// A passing test therefore proves council was skipped.

#[tokio::test]
async fn trivial_skips_council() {
    let repo = temp_repo();
    let quota = store();

    let conductor = std::sync::Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(Provider::Claude, r#"{"complexity":"trivial"}"#),
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "create file", "create auto_trivial.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = std::sync::Arc::new(ScriptedAdapter {
        pre_script: "echo 'trivial' > auto_trivial.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "created auto_trivial.txt")
    });

    // Failing council member: if council runs, run_council bails → run_auto Err.
    let bad_council_member = ScriptedAdapter::failing(Provider::Codex, "should not be called");

    let deps = AutoDeps {
        conduct: ConductDeps {
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
            reviewer: None,
            arbiter: None,
        },
        council_members: vec![CouncilMember {
            label: "bad-member".into(),
            adapter: std::sync::Arc::new(bad_council_member),
            model: None,
        }],
        chairman: RoleHandle {
            adapter: std::sync::Arc::new(ScriptedAdapter::failing(
                Provider::Claude,
                "chairman should not be called",
            )),
            model: None,
        },
    };

    let outcome = run_auto(
        "create a trivial file",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        None,
    )
    .await
    .expect("trivial auto run should succeed");

    assert!(outcome.triage_trivial, "triage should be trivial");
    assert!(
        outcome.council_synthesis.is_none(),
        "council should be skipped for trivial tasks"
    );
    assert!(
        !outcome.conduct.completed.is_empty(),
        "conduct should complete"
    );
}

// ─── Test 2: standard_runs_council_then_conduct ──────────────────────────────
//
// Conductor sequence: [triage "standard", plan(1 subtask), accept].
// Council members/chairman scripted ok.
// Assert: council_synthesis.is_some(), conduct.completed == [1].

#[tokio::test]
async fn standard_runs_council_then_conduct() {
    let repo = temp_repo();
    let quota = store();

    let conductor = std::sync::Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(Provider::Claude, r#"{"complexity":"standard"}"#),
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "create file", "create auto_std.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = std::sync::Arc::new(ScriptedAdapter {
        pre_script: "echo 'standard' > auto_std.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "created auto_std.txt")
    });

    // Council: one answering member + chairman synthesizes.
    let council_member_adapter = std::sync::Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        "Here is my plan: start with the file.",
    ));
    let chairman_adapter = std::sync::Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        &council_ok_json("Synthesized plan: create the file directly."),
    ));

    let deps = AutoDeps {
        conduct: ConductDeps {
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
            reviewer: None,
            arbiter: None,
        },
        council_members: vec![CouncilMember {
            label: "claude-council".into(),
            adapter: council_member_adapter,
            model: None,
        }],
        chairman: RoleHandle {
            adapter: chairman_adapter,
            model: None,
        },
    };

    let outcome = run_auto(
        "create a standard file",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        None,
    )
    .await
    .expect("standard auto run should succeed");

    assert!(!outcome.triage_trivial, "triage should be standard");
    assert!(
        outcome.council_synthesis.is_some(),
        "council should run for standard tasks"
    );
    assert_eq!(outcome.conduct.completed, vec![1]);
    assert!(outcome.conduct.halted.is_none());
    assert!(outcome.conduct.failed.is_none());
}

// ─── Test 3: check_command_failure_reported ───────────────────────────────────
//
// Trivial flow + check_command = Some("false") → check == Some((false, _)).

#[tokio::test]
async fn check_command_failure_reported() {
    let repo = temp_repo();
    let quota = store();

    let conductor = std::sync::Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(Provider::Claude, r#"{"complexity":"trivial"}"#),
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "create file", "create check_fail.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = std::sync::Arc::new(ScriptedAdapter {
        pre_script: "echo 'check' > check_fail.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "created check_fail.txt")
    });

    let deps = AutoDeps {
        conduct: ConductDeps {
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
            reviewer: None,
            arbiter: None,
        },
        council_members: vec![],
        chairman: RoleHandle {
            adapter: std::sync::Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Claude,
                "chairman not called for trivial",
            )),
            model: None,
        },
    };

    let outcome = run_auto(
        "create a file then check fails",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        Some("false"), // `false` always exits 1
    )
    .await
    .expect("auto run should succeed even when check fails");

    let check = outcome
        .check
        .expect("check should be Some when command given");
    assert!(!check.0, "check should report failure (exit code 1)");
    // output tail may be empty for `false` but Some((_,_)) is the contract
}

// ─── Test 4: check_command_success ────────────────────────────────────────────
//
// Trivial flow + check_command = Some("true") → check == Some((true, _)).

#[tokio::test]
async fn check_command_success() {
    let repo = temp_repo();
    let quota = store();

    let conductor = std::sync::Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(Provider::Claude, r#"{"complexity":"trivial"}"#),
            ScriptedAdapter::ok_with_text(
                Provider::Claude,
                &plan_json(&[(1, "create file", "create check_ok.txt")]),
            ),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));

    let worker = std::sync::Arc::new(ScriptedAdapter {
        pre_script: "echo 'check' > check_ok.txt".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "created check_ok.txt")
    });

    let deps = AutoDeps {
        conduct: ConductDeps {
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
            reviewer: None,
            arbiter: None,
        },
        council_members: vec![],
        chairman: RoleHandle {
            adapter: std::sync::Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Claude,
                "chairman not called for trivial",
            )),
            model: None,
        },
    };

    let outcome = run_auto(
        "create a file then check passes",
        deps,
        &quota,
        repo.path().to_path_buf(),
        TIMEOUT,
        Some("true"), // `true` always exits 0
    )
    .await
    .expect("auto run should succeed");

    let check = outcome
        .check
        .expect("check should be Some when command given");
    assert!(check.0, "check should report success (exit code 0)");
}
