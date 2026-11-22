//! Cross-examination end to end, with scripted stand-ins for the provider
//! CLIs: who rules on a reviewer's findings, and what happens when no one can.

mod common;

use common::{RecordingAdapter, ScriptedAdapter};
use consilium::config::ModelCandidate;
use consilium::event::Provider;
use consilium::family::ModelFamily;
use consilium::orchestrator::council::CouncilMember;
use consilium::orchestrator::cross_examine::{examine_review, Independence, Parties, Ruling};
use consilium::orchestrator::resilience::{ModelHealth, Rung};
use consilium::orchestrator::review::{parse_verdict, Finding};
use consilium::quota::QuotaStore;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type CallLog = Arc<Mutex<Vec<(String, bool, bool)>>>;

const DIFF: &str = "diff --git a/avg.rs b/avg.rs\n+let avg = sum / n;";

const RULINGS: &str = r#"```json
{"rulings":[{"finding":1,"ruling":"confirmed","reason":"n is 0 for an empty slice"},{"finding":2,"ruling":"refuted","reason":"the name matches what it returns"}]}
```"#;

fn findings() -> Vec<Finding> {
    parse_verdict(
        r#"{"findings":[{"severity":"critical","file":"avg.rs","description":"divides by zero"},{"severity":"important","file":"avg.rs","description":"misleading name"},{"severity":"minor","file":"avg.rs","description":"typo in a comment"}]}"#,
    )
    .expect("test findings parse")
    .findings
}

fn rung(
    provider: Provider,
    model: &str,
    adapter: impl consilium::adapters::Adapter + 'static,
) -> Rung {
    Rung {
        candidate: ModelCandidate {
            provider,
            model: model.into(),
        },
        adapter: Arc::new(adapter),
    }
}

fn worker(label: &str, rung: Rung) -> CouncilMember {
    CouncilMember {
        label: label.into(),
        ladder: vec![rung],
    }
}

/// A model that answers `reply` and records every prompt it is given.
fn recorded(provider: Provider, reply: &str) -> (RecordingAdapter, CallLog) {
    let log: CallLog = Arc::default();
    let adapter =
        RecordingAdapter::new(ScriptedAdapter::ok_with_text(provider, reply), log.clone());
    (adapter, log)
}

async fn examined_by(
    reviewer_ladder: &[Rung],
    workers: &[CouncilMember],
    findings: &[Finding],
) -> anyhow::Result<Option<consilium::orchestrator::cross_examine::CrossExamined>> {
    let parties = Parties {
        reviewer_ladder,
        workers,
        reviewer_model: Some("codex/gpt-6-luna"),
        author: ModelFamily::Anthropic,
        independent_only: false,
        task: None,
    };
    examine_review(
        DIFF,
        findings,
        &parties,
        &ModelHealth::new(),
        &QuotaStore::open_in_memory().unwrap(),
        std::env::temp_dir(),
        Duration::from_secs(20),
    )
    .await
}

/// Codex reviewed a diff Claude wrote; the findings go to Gemini, which has a
/// stake in neither — read-only, with the findings and the diff in hand.
#[tokio::test]
async fn findings_are_ruled_on_by_a_family_that_neither_wrote_nor_reviewed() {
    let (reviewer, reviewer_calls) = recorded(Provider::Codex, "unused");
    let (author_side, author_calls) = recorded(Provider::Claude, "unused");
    let (third, third_calls) = recorded(Provider::Gemini, RULINGS);
    let reviewer_ladder = [rung(Provider::Codex, "gpt-6-luna", reviewer)];
    let workers = [
        worker(
            "claude",
            rung(Provider::Claude, "claude-fable-5-2", author_side),
        ),
        worker(
            "gemini",
            rung(Provider::Gemini, "gemini-4-argon-high", third),
        ),
    ];

    let examined = examined_by(&reviewer_ladder, &workers, &findings())
        .await
        .unwrap()
        .expect("there are findings to examine");

    assert_eq!(examined.examiner, "gemini/gemini-4-argon-high");
    assert_eq!(examined.independence, Independence::Independent);
    let rulings: Vec<Option<Ruling>> = examined
        .rulings
        .iter()
        .map(|ruled| ruled.as_ref().map(|r| r.ruling))
        .collect();
    assert_eq!(
        rulings,
        [Some(Ruling::Confirmed), Some(Ruling::Refuted), None],
        "the minor finding is not examined"
    );
    assert_eq!(
        examined.rulings[0].as_ref().unwrap().reason,
        "n is 0 for an empty slice"
    );

    let calls = third_calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "one call rules on every finding");
    let (prompt, advisory, write) = &calls[0];
    assert!(
        prompt.contains("1. [critical] avg.rs — divides by zero"),
        "{prompt}"
    );
    assert!(
        prompt.contains("2. [important] avg.rs — misleading name"),
        "{prompt}"
    );
    assert!(
        !prompt.contains("typo in a comment"),
        "minor findings stay out: {prompt}"
    );
    assert!(prompt.contains(DIFF), "{prompt}");
    assert!(*advisory && !*write, "an examiner never changes a file");
    assert!(reviewer_calls.lock().unwrap().is_empty());
    assert!(author_calls.lock().unwrap().is_empty());
}

/// Asking the model that wrote the findings whether they are right proves
/// nothing. With no one else in the lineup there is no examination.
#[tokio::test]
async fn the_model_that_reviewed_never_examines_its_own_findings() {
    let (reviewer, reviewer_calls) = recorded(Provider::Codex, RULINGS);
    let reviewer_ladder = [rung(Provider::Codex, "gpt-6-luna", reviewer)];

    let error = examined_by(&reviewer_ladder, &[], &findings())
        .await
        .unwrap_err();

    assert!(error.to_string().contains("no other model"), "{error}");
    assert!(reviewer_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_reply_without_rulings_fails_the_examination() {
    let (third, _) = recorded(Provider::Gemini, "They all look plausible to me.");
    let reviewer_ladder = [rung(
        Provider::Codex,
        "gpt-6-luna",
        ScriptedAdapter::ok_with_text(Provider::Codex, "unused"),
    )];
    let workers = [worker(
        "gemini",
        rung(Provider::Gemini, "gemini-4-argon-high", third),
    )];

    let error = examined_by(&reviewer_ladder, &workers, &findings())
        .await
        .unwrap_err();

    assert!(error.to_string().contains("no rulings"), "{error}");
}

#[tokio::test]
async fn a_clean_review_is_not_examined() {
    let (third, third_calls) = recorded(Provider::Gemini, RULINGS);
    let reviewer_ladder = [rung(
        Provider::Codex,
        "gpt-6-luna",
        ScriptedAdapter::ok_with_text(Provider::Codex, "unused"),
    )];
    let workers = [worker(
        "gemini",
        rung(Provider::Gemini, "gemini-4-argon-high", third),
    )];

    let examined = examined_by(&reviewer_ladder, &workers, &[]).await.unwrap();

    assert!(examined.is_none());
    assert!(third_calls.lock().unwrap().is_empty(), "nothing to rule on");
}

/// Minor findings gate nothing and are judged at a glance: a review with
/// nothing worse is not worth a model call.
#[tokio::test]
async fn a_review_with_only_minor_findings_is_not_examined() {
    let (third, third_calls) = recorded(Provider::Gemini, RULINGS);
    let reviewer_ladder = [rung(
        Provider::Codex,
        "gpt-6-luna",
        ScriptedAdapter::ok_with_text(Provider::Codex, "unused"),
    )];
    let workers = [worker(
        "gemini",
        rung(Provider::Gemini, "gemini-4-argon-high", third),
    )];
    let minor =
        parse_verdict(r#"{"findings":[{"severity":"minor","file":"a.rs","description":"typo"}]}"#)
            .unwrap()
            .findings;

    let examined = examined_by(&reviewer_ladder, &workers, &minor)
        .await
        .unwrap();

    assert!(examined.is_none());
    assert!(third_calls.lock().unwrap().is_empty());
}

/// An examiner that is down does not cost the examination: the next most
/// independent model rules, and the result says which one it was.
#[tokio::test]
async fn a_dead_examiner_hands_over_to_the_next() {
    let dead = ScriptedAdapter::failing(Provider::Gemini, "invalid model selection");
    let (author_side, _) = recorded(Provider::Claude, RULINGS);
    let reviewer_ladder = [rung(
        Provider::Codex,
        "gpt-6-luna",
        ScriptedAdapter::ok_with_text(Provider::Codex, "unused"),
    )];
    let workers = [
        worker("gemini", rung(Provider::Gemini, "gemini-retired", dead)),
        worker(
            "claude",
            rung(Provider::Claude, "claude-fable-5-2", author_side),
        ),
    ];

    let examined = examined_by(&reviewer_ladder, &workers, &findings())
        .await
        .unwrap()
        .expect("there are findings to examine");

    assert_eq!(examined.examiner, "claude/claude-fable-5-2");
    assert_eq!(
        examined.independence,
        Independence::AuthorsFamily,
        "the standing is the standing of the model that ruled"
    );
    assert_eq!(examined.fallbacks.len(), 1);
}

/// The reviewer is now told the task and reports what the diff fails to do.
/// An examiner that sees only the diff would call that a misreading.
#[tokio::test]
async fn the_examiner_is_told_what_the_diff_was_meant_to_do() {
    let (third, third_calls) = recorded(Provider::Gemini, RULINGS);
    let reviewer_ladder = [rung(
        Provider::Codex,
        "gpt-6-luna",
        ScriptedAdapter::ok_with_text(Provider::Codex, "unused"),
    )];
    let workers = [worker(
        "gemini",
        rung(Provider::Gemini, "gemini-4-argon-high", third),
    )];
    let parties = Parties {
        reviewer_ladder: &reviewer_ladder,
        workers: &workers,
        reviewer_model: Some("codex/gpt-6-luna"),
        author: ModelFamily::Anthropic,
        independent_only: false,
        task: Some("average the samples, skipping empty batches"),
    };

    examine_review(
        DIFF,
        &findings(),
        &parties,
        &ModelHealth::new(),
        &QuotaStore::open_in_memory().unwrap(),
        std::env::temp_dir(),
        Duration::from_secs(20),
    )
    .await
    .unwrap();

    let calls = third_calls.lock().unwrap();
    assert!(
        calls[0]
            .0
            .contains("<task>\naverage the samples, skipping empty batches\n</task>"),
        "{}",
        calls[0].0
    );
}

/// Where the ruling decides something by itself, only a model with no stake
/// is asked: the reviewer's other rung is often the reviewer itself under
/// another name, and the author's side defends its own diff.
#[tokio::test]
async fn where_only_independence_counts_no_one_else_is_asked() {
    let (same_side, same_side_calls) = recorded(Provider::Codex, RULINGS);
    let (author_side, author_calls) = recorded(Provider::Claude, RULINGS);
    let reviewer_ladder = [
        rung(
            Provider::Codex,
            "gpt-6-luna",
            ScriptedAdapter::ok_with_text(Provider::Codex, "unused"),
        ),
        rung(Provider::Codex, "default", same_side),
    ];
    let workers = [worker(
        "claude",
        rung(Provider::Claude, "claude-fable-5-2", author_side),
    )];
    let parties = Parties {
        reviewer_ladder: &reviewer_ladder,
        workers: &workers,
        reviewer_model: Some("codex/gpt-6-luna"),
        author: ModelFamily::Anthropic,
        independent_only: true,
        task: None,
    };

    let error = examine_review(
        DIFF,
        &findings(),
        &parties,
        &ModelHealth::new(),
        &QuotaStore::open_in_memory().unwrap(),
        std::env::temp_dir(),
        Duration::from_secs(20),
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("no other model"), "{error}");
    assert!(same_side_calls.lock().unwrap().is_empty());
    assert!(author_calls.lock().unwrap().is_empty());
}
