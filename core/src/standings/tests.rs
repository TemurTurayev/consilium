use super::names::model_name;
use super::*;
use serde_json::{json, Value};

// ── builders ────────────────────────────────────────────────────────────────

/// A council of `answers` (anonymous label, member label, model used) scored
/// by `scores` (scoring member, [(anonymous label, score)]).
fn council(answers: &[(&str, &str, Option<&str>)], scores: &[(&str, &[(&str, u8)])]) -> Value {
    json!({
        "kind": "council",
        "answers": answers.iter().map(|(anon, member, model)| json!({
            "anon_label": anon, "member": member, "model_used": model, "text": "…"
        })).collect::<Vec<_>>(),
        "scores": scores.iter().map(|(member, given)| json!({
            "member": member,
            "parsed": given.iter().map(|(agent, score)| json!({
                "agent": agent, "score": score, "justification": ""
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn review(reviewer: &str, examiner: Option<&str>, rulings: &[Option<&str>]) -> Value {
    json!({
        "kind": "review",
        "model_used": reviewer,
        "examined_by": examiner,
        "examiner_independence": examiner.map(|_| "independent"),
        "findings": rulings.iter().map(|ruling| json!({
            "severity": "important", "file": "a.rs", "description": "d", "ruling": ruling
        })).collect::<Vec<_>>(),
    })
}

/// A `conduct` run with one subtask per entry: (status, [(worker, checks)]).
fn conduct(subtasks: &[(&str, &[(&str, &str)])]) -> Value {
    json!({
        "kind": "conduct",
        "subtasks": subtasks.iter().map(|(status, attempts)| json!({
            "status": status,
            "attempts": attempts.iter().map(|(worker, checks)| json!({
                "worker": worker, "verify": checks, "decision": "accept"
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn row<'a, T>(rows: &'a [T], model: &str, name: impl Fn(&T) -> &str) -> &'a T {
    rows.iter()
        .find(|row| name(row) == model)
        .unwrap_or_else(|| panic!("no row for {model}"))
}

// ── names ───────────────────────────────────────────────────────────────────

#[test]
fn a_model_is_named_by_what_ran_or_else_by_its_label() {
    assert_eq!(
        model_name(
            Some("gemini/claude-opus-5-5-high"),
            "gemini-claude-opus-5-5-high"
        ),
        "gemini/claude-opus-5-5-high"
    );
    // runs saved before the model was recorded carry only the label
    assert_eq!(model_name(None, "codex-gpt-5.5"), "codex/gpt-5.5");
    assert_eq!(
        model_name(None, "gemini-Gemini 3.1 Pro (High)"),
        "gemini/Gemini 3.1 Pro (High)"
    );
    // a duplicate worker's label is the same model
    assert_eq!(
        model_name(None, "claude-claude-fable-5-2#2"),
        "claude/claude-fable-5-2"
    );
    // a label that names no provider is kept as it is
    assert_eq!(model_name(None, "codex-worker"), "codex/worker");
    assert_eq!(model_name(None, "reviewer"), "reviewer");
    assert_eq!(model_name(Some("  "), "grok-default"), "grok/default");
}

// ── council ─────────────────────────────────────────────────────────────────

/// A member scores every answer, its own included. Its opinion of itself is
/// not the other members' opinion of it.
#[test]
fn a_council_answer_is_scored_by_the_other_members_only() {
    let run = council(
        &[("A", "claude-fable", None), ("B", "codex-gpt", None)],
        &[
            ("claude-fable", &[("A", 10), ("B", 4)]),
            ("codex-gpt", &[("A", 6), ("B", 10)]),
        ],
    );
    let standings = tally(&[run]);
    let fable = row(&standings.council, "claude/fable", |r| &r.model);
    let gpt = row(&standings.council, "codex/gpt", |r| &r.model);
    assert_eq!(
        (fable.answers, fable.scored, fable.peer_score),
        (1, 1, Some(6.0))
    );
    assert_eq!((gpt.answers, gpt.scored, gpt.peer_score), (1, 1, Some(4.0)));
    assert_eq!((fable.best_answer, gpt.best_answer), (1, 0));
}

#[test]
fn peer_scores_are_averaged_over_a_models_answers() {
    let first = council(
        &[
            ("A", "claude-fable", None),
            ("B", "codex-gpt", None),
            ("C", "gemini-argon", None),
        ],
        &[
            ("codex-gpt", &[("A", 8), ("C", 5)]),
            ("gemini-argon", &[("A", 6), ("B", 5)]),
        ],
    );
    let second = council(
        &[("A", "codex-gpt", None), ("B", "claude-fable", None)],
        &[("codex-gpt", &[("B", 9)])],
    );
    let standings = tally(&[first, second]);
    let fable = row(&standings.council, "claude/fable", |r| &r.model);
    // (8 + 6) / 2 = 7 in the first council — ahead of 5 and 5 — and 9 in the second
    assert_eq!(
        (fable.answers, fable.scored, fable.peer_score),
        (2, 2, Some(8.0))
    );
    assert_eq!(
        fable.best_answer, 1,
        "the only scored answer of a council wins nothing"
    );
    assert_eq!(
        standings.council[0].model, "claude/fable",
        "best score first"
    );
}

#[test]
fn an_answer_nobody_else_scored_has_no_score() {
    let run = council(
        &[("A", "claude-fable", None), ("B", "codex-gpt", None)],
        &[("claude-fable", &[("A", 9)])],
    );
    let standings = tally(&[run]);
    let fable = row(&standings.council, "claude/fable", |r| &r.model);
    assert_eq!(
        (fable.answers, fable.scored, fable.peer_score),
        (1, 0, None)
    );
}

#[test]
fn a_tie_for_the_best_answer_is_nobodys_win() {
    let run = council(
        &[("A", "claude-fable", None), ("B", "codex-gpt", None)],
        &[("claude-fable", &[("B", 7)]), ("codex-gpt", &[("A", 7)])],
    );
    let standings = tally(&[run]);
    assert!(standings.council.iter().all(|r| r.best_answer == 0));
}

/// Reviewers write the label as "A", "Agent A" or "a".
#[test]
fn scores_find_their_answer_however_the_reviewer_wrote_the_label() {
    let run = council(
        &[("A", "claude-fable", None), ("B", "codex-gpt", None)],
        &[
            ("codex-gpt", &[("Agent A", 8)]),
            ("claude-fable", &[("b", 3)]),
        ],
    );
    let standings = tally(&[run]);
    assert_eq!(
        row(&standings.council, "claude/fable", |r| &r.model).peer_score,
        Some(8.0)
    );
    assert_eq!(
        row(&standings.council, "codex/gpt", |r| &r.model).peer_score,
        Some(3.0)
    );
}

/// Two workers on one model are one model: what one makes of the other's
/// answer is that model's opinion of itself.
#[test]
fn a_duplicate_worker_does_not_score_its_own_model() {
    let run = council(
        &[
            ("A", "codex-gpt", None),
            ("B", "codex-gpt#2", None),
            ("C", "claude-fable", None),
        ],
        &[
            ("codex-gpt", &[("B", 10), ("C", 5)]),
            ("codex-gpt#2", &[("A", 10), ("C", 5)]),
            ("claude-fable", &[("A", 6), ("B", 6)]),
        ],
    );
    let standings = tally(&[run]);
    let gpt = row(&standings.council, "codex/gpt", |r| &r.model);
    assert_eq!((gpt.answers, gpt.scored, gpt.peer_score), (2, 2, Some(6.0)));
}

/// A member whose answer came from a fallback model scores as that model.
#[test]
fn a_member_scores_as_the_model_that_answered_for_it() {
    let run = council(
        &[
            ("A", "gemini-argon", Some("claude/claude-opus-5-5")),
            ("B", "claude-opus", Some("claude/claude-opus-5-5")),
        ],
        &[
            ("gemini-argon", &[("B", 10)]),
            ("claude-opus", &[("A", 10)]),
        ],
    );
    let standings = tally(&[run]);
    let opus = row(&standings.council, "claude/claude-opus-5-5", |r| &r.model);
    assert_eq!((opus.answers, opus.scored, opus.peer_score), (2, 0, None));
}

/// A member's review can run on another model than its answer did (a
/// fallback). When the run says which, that is the model whose opinion it is.
#[test]
fn a_score_belongs_to_the_model_that_gave_it() {
    let run = json!({
        "kind": "council",
        "answers": [
            {"anon_label": "A", "member": "claude-fable", "model_used": "claude/claude-fable-5-2"},
            {"anon_label": "B", "member": "codex-gpt", "model_used": "codex/gpt-6-luna"},
        ],
        "scores": [
            // codex's review fell back to the model that wrote answer A
            {"member": "codex-gpt", "model_used": "claude/claude-fable-5-2",
             "parsed": [{"agent": "A", "score": 10}]},
            {"member": "claude-fable", "model_used": "claude/claude-fable-5-2",
             "parsed": [{"agent": "B", "score": 4}]},
        ],
    });
    let standings = tally(&[run]);
    assert_eq!(
        row(&standings.council, "claude/claude-fable-5-2", |r| &r.model).scored,
        0
    );
    assert_eq!(
        row(&standings.council, "codex/gpt-6-luna", |r| &r.model).peer_score,
        Some(4.0)
    );
}

/// A score that names no scorer cannot be told from a self-score.
#[test]
fn a_score_from_nobody_is_not_counted() {
    let run = json!({
        "kind": "council",
        "answers": [{"anon_label": "A", "member": "claude-fable"}],
        "scores": [{"parsed": [{"agent": "A", "score": 10}]}],
    });
    let standings = tally(&[run]);
    assert_eq!(
        row(&standings.council, "claude/fable", |r| &r.model).scored,
        0
    );
}

// ── reviews ─────────────────────────────────────────────────────────────────

#[test]
fn a_reviewer_is_counted_by_what_became_of_its_findings() {
    let runs = [
        review(
            "codex/gpt-6-luna",
            Some("gemini/gemini-4-argon-high"),
            &[Some("confirmed"), Some("refuted"), Some("unclear"), None],
        ),
        review(
            "codex/gpt-6-luna",
            Some("gemini/gemini-4-argon-high"),
            &[Some("confirmed")],
        ),
    ];
    let standings = tally(&runs);
    let reviewer = row(&standings.reviewers, "codex/gpt-6-luna", |r| &r.model);
    assert_eq!(
        (
            reviewer.reviews,
            reviewer.confirmed,
            reviewer.refuted,
            reviewer.unclear
        ),
        (2, 2, 1, 1)
    );
    assert_eq!(reviewer.held_up, Some(2.0 / 3.0));
    let examiner = row(&standings.examiners, "gemini/gemini-4-argon-high", |r| {
        &r.model
    });
    assert_eq!(
        (
            examiner.examinations,
            examiner.confirmed,
            examiner.refuted,
            examiner.unclear
        ),
        (2, 2, 1, 1)
    );
}

/// Reviews saved before findings were ruled on still count as reviews — and
/// say nothing about how the findings held up.
#[test]
fn an_unexamined_review_counts_as_a_review_and_nothing_more() {
    let old = json!({"kind": "review", "model_used": "codex/gpt-5.5", "raw_review": "…"});
    let standings = tally(&[old]);
    let reviewer = row(&standings.reviewers, "codex/gpt-5.5", |r| &r.model);
    assert_eq!(
        (reviewer.reviews, reviewer.confirmed, reviewer.refuted),
        (1, 0, 0)
    );
    assert_eq!(reviewer.held_up, None);
    assert!(standings.examiners.is_empty());
}

/// A reviewer's record rests on rulings by a model with no stake. One from
/// the author's side or the reviewer's own is reported where it was given,
/// but says too little about the reviewer to count for or against it.
#[test]
fn only_independent_rulings_count_towards_a_reviewers_record() {
    let stake = |independence: &str| {
        json!({
            "kind": "review",
            "model_used": "codex/gpt",
            "examined_by": "claude/fable",
            "examiner_independence": independence,
            "findings": [{"ruling": "refuted"}, {"ruling": "confirmed"}],
        })
    };
    let standings = tally(&[
        stake("authors_family"),
        stake("reviewers_family"),
        stake("unknown_author"),
    ]);
    let reviewer = row(&standings.reviewers, "codex/gpt", |r| &r.model);
    assert_eq!(
        (reviewer.reviews, reviewer.confirmed, reviewer.refuted),
        (3, 0, 0)
    );
    assert_eq!(reviewer.held_up, None);
    let examiner = row(&standings.examiners, "claude/fable", |r| &r.model);
    assert_eq!(
        (examiner.examinations, examiner.confirmed, examiner.refuted),
        (3, 3, 3)
    );

    let independent = tally(&[stake("independent")]);
    let reviewer = row(&independent.reviewers, "codex/gpt", |r| &r.model);
    assert_eq!((reviewer.confirmed, reviewer.refuted), (1, 1));
}

/// `conduct` records the same on each attempt its review gate looked at.
#[test]
fn reviews_inside_a_conduct_run_are_counted_too() {
    let run = json!({
        "kind": "conduct",
        "subtasks": [{"status": "completed", "attempts": [{
            "worker": "codex-gpt",
            "reviewer_model": "claude/claude-opus-5-5",
            "examined_by": "gemini/gemini-4-argon-high",
            "examiner_independence": "independent",
            "findings": [{"severity": "critical", "ruling": "refuted"}],
        }, {
            "worker": "codex-gpt",
            // an attempt the review gate never reached
        }]}],
    });
    let standings = tally(&[run]);
    let reviewer = row(&standings.reviewers, "claude/claude-opus-5-5", |r| &r.model);
    assert_eq!((reviewer.reviews, reviewer.refuted), (1, 1));
    let examiner = row(&standings.examiners, "gemini/gemini-4-argon-high", |r| {
        &r.model
    });
    assert_eq!((examiner.examinations, examiner.refuted), (1, 1));
}

// ── workers ─────────────────────────────────────────────────────────────────

#[test]
fn a_worker_is_counted_by_subtasks_accepted_first_try() {
    let run = conduct(&[
        ("completed", &[("codex-gpt", "passed")]),
        (
            "completed",
            &[("codex-gpt", "failed"), ("codex-gpt", "passed")],
        ),
        (
            "failed",
            &[("gemini-argon", "failed"), ("gemini-argon", "failed")],
        ),
        ("skipped", &[]),
    ]);
    let standings = tally(&[run]);
    let gpt = row(&standings.workers, "codex/gpt", |r| &r.model);
    assert_eq!(
        (gpt.subtasks, gpt.first_try, gpt.reworks, gpt.checks_failed),
        (2, 1, 1, 1)
    );
    let argon = row(&standings.workers, "gemini/argon", |r| &r.model);
    assert_eq!(
        (
            argon.subtasks,
            argon.first_try,
            argon.reworks,
            argon.checks_failed
        ),
        (1, 0, 1, 2)
    );
}

/// A `run_worker` call is one subtask: accepted first try unless the worker
/// failed or the project's checks did.
#[test]
fn a_delegated_run_is_a_subtask() {
    let runs = [
        json!({"kind": "worker", "worker": "gemini-argon", "model_used": "gemini/argon",
               "verify": {"passed": true}}),
        json!({"kind": "worker", "worker": "gemini-argon", "model_used": "gemini/argon",
               "verify": {"passed": false}}),
        json!({"kind": "worker", "worker": "gemini-argon", "model_used": "gemini/argon",
               "verify": null}),
        json!({"kind": "worker", "worker": "gemini-argon", "failed": "timed out"}),
    ];
    let standings = tally(&runs);
    let argon = row(&standings.workers, "gemini/argon", |r| &r.model);
    assert_eq!(
        (argon.subtasks, argon.first_try, argon.checks_failed),
        (4, 2, 1)
    );
}

// ── the whole ───────────────────────────────────────────────────────────────

#[test]
fn every_run_is_counted_and_unknown_kinds_change_nothing_else() {
    let runs = [json!({"kind": "auto"}), json!({"no": "kind"}), json!(7)];
    let standings = tally(&runs);
    assert_eq!(standings.runs, 3);
    assert!(standings.council.is_empty() && standings.workers.is_empty());
    assert!(standings.nominations.is_empty());
}

/// A rate over a handful of results says nothing: no one is named for it.
#[test]
fn nobody_is_nominated_on_too_little() {
    let four_confirmed = [Some("confirmed"); 4];
    let runs = [
        review("codex/gpt", Some("gemini/argon"), &four_confirmed),
        conduct(&[("completed", &[("codex-gpt", "passed")])]),
    ];
    assert!(tally(&runs).nominations.is_empty());
}

/// "Best" among one is nobody standing out: with a single model that has
/// enough to go on, the comparison has no other side.
#[test]
fn a_lone_candidate_is_not_named_best() {
    let councils: Vec<Value> = (0..MIN_SAMPLE)
        .map(|round| {
            // the same model every round, against a different one each time
            let rival = format!("codex-gpt-{round}");
            council(
                &[("A", "claude-fable", None), ("B", rival.as_str(), None)],
                &[("claude-fable", &[("B", 9)]), (rival.as_str(), &[("A", 3)])],
            )
        })
        .collect();
    let passed: &[(&str, &str)] = &[("gemini-argon", "passed")];
    let five = [Some("confirmed"); 5];
    let runs: Vec<Value> = councils
        .into_iter()
        .chain([
            review("codex/gpt", Some("gemini/argon"), &five),
            conduct(&[("completed", passed); 5]),
        ])
        .collect();

    let titles: Vec<String> = tally(&runs)
        .nominations
        .into_iter()
        .map(|n| n.title)
        .collect();
    assert!(titles.is_empty(), "{titles:?}");
}

#[test]
fn nominations_name_who_stands_out_and_why() {
    let councils: Vec<Value> = (0..MIN_SAMPLE)
        .map(|_| {
            council(
                &[("A", "claude-fable", None), ("B", "codex-gpt", None)],
                &[("claude-fable", &[("B", 5)]), ("codex-gpt", &[("A", 9)])],
            )
        })
        .collect();
    let mostly = |often: &'static str, once: &'static str| {
        [
            Some(often),
            Some(often),
            Some(often),
            Some(often),
            Some(once),
        ]
    };
    let reviews = [
        review(
            "codex/gpt",
            Some("gemini/argon"),
            &mostly("confirmed", "refuted"),
        ),
        review(
            "claude/opus",
            Some("gemini/argon"),
            &mostly("refuted", "confirmed"),
        ),
    ];
    let passed: &[(&str, &str)] = &[("gemini-argon", "passed")];
    let reworked: &[(&str, &str)] = &[("codex-gpt", "failed"), ("codex-gpt", "passed")];
    let work = [
        conduct(&[("completed", passed); 5]),
        conduct(&[("completed", reworked); 5]),
    ];
    let runs: Vec<Value> = councils.into_iter().chain(reviews).chain(work).collect();

    let nominations = tally(&runs).nominations;
    let named = |title: &str| {
        nominations
            .iter()
            .find(|n| n.title == title)
            .map(|n| (n.model.as_str(), n.because.as_str()))
    };
    assert_eq!(
        named("Best answers"),
        Some((
            "claude/fable",
            "9.0/10 from the other members over 5 answers"
        ))
    );
    assert_eq!(
        named("Sharpest eye"),
        Some(("codex/gpt", "4 of 5 findings confirmed by a second model"))
    );
    assert_eq!(
        named("Most false alarms"),
        Some(("claude/opus", "4 of 5 findings refuted by a second model"))
    );
    assert_eq!(
        named("Cleanest work"),
        Some(("gemini/argon", "5 of 5 subtasks accepted first try"))
    );
}

#[test]
fn the_table_shows_each_section_that_has_rows_and_how_much_it_rests_on() {
    let runs = [
        council(
            &[("A", "claude-fable", None), ("B", "codex-gpt", None)],
            &[("codex-gpt", &[("A", 8)])],
        ),
        review(
            "codex/gpt",
            Some("gemini/argon"),
            &[Some("confirmed"), Some("refuted")],
        ),
    ];
    let text = render(&tally(&runs));
    assert!(text.contains("from 2 saved runs"), "{text}");
    assert!(text.contains("claude/fable"), "{text}");
    assert!(text.contains("8.0"), "{text}");
    assert!(text.contains("50%"), "{text}");
    assert!(
        !text.contains("workers"),
        "an empty section is not printed: {text}"
    );
}

#[test]
fn no_runs_is_said_plainly() {
    let text = render(&tally(&[]));
    assert!(text.contains("no saved runs"), "{text}");
}

// ── reading the saved runs ──────────────────────────────────────────────────

#[test]
fn saved_runs_are_read_oldest_first_skipping_old_and_broken_ones() {
    let base = tempfile::tempdir().unwrap();
    let runs = base.path().join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let hour = 3_600_000_000_000u128;
    let save = |nanos: u128, kind: &str, body: &str| {
        std::fs::write(runs.join(format!("{nanos}-0000-{kind}.json")), body).unwrap();
    };
    save(now - hour, "review", r#"{"kind":"review","n":2}"#);
    save(now - 2 * hour, "council", r#"{"kind":"council","n":1}"#);
    save(now - 50 * hour, "council", r#"{"kind":"council","n":0}"#);
    save(now - 3 * hour, "conduct", "{ not json");
    std::fs::write(runs.join("notes.txt"), "not a run").unwrap();

    let day = std::time::Duration::from_secs(24 * 3600);
    let recent: Vec<u64> = load(base.path(), Some(day))
        .iter()
        .filter_map(|run| run["n"].as_u64())
        .collect();
    assert_eq!(recent, [1, 2]);

    let all: Vec<u64> = load(base.path(), None)
        .iter()
        .filter_map(|run| run["n"].as_u64())
        .collect();
    assert_eq!(all, [0, 1, 2]);

    assert!(load(&base.path().join("nowhere"), None).is_empty());
}
