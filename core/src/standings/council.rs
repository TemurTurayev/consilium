//! Council answers, as the other members scored them.

use super::names::{is_kind, list, model_name, text};
use super::CouncilRow;
use serde_json::Value;
use std::collections::BTreeMap;

/// The highest score a council reviewer can give.
const TOP_SCORE: f64 = 10.0;

/// What one answer got in one council.
struct Scored {
    model: String,
    /// Mean of the other members' scores; `None` when no one else scored it.
    peer_score: Option<f64>,
    /// Scored highest outright among that council's answers.
    best: bool,
}

pub(super) fn rows(runs: &[Value]) -> Vec<CouncilRow> {
    let by_model = runs
        .iter()
        .filter(|run| is_kind(run, "council"))
        .flat_map(scored_answers)
        .fold(
            BTreeMap::<String, Vec<Scored>>::new(),
            |mut by_model, scored| {
                by_model
                    .entry(scored.model.clone())
                    .or_default()
                    .push(scored);
                by_model
            },
        );
    let mut rows: Vec<CouncilRow> = by_model
        .into_iter()
        .map(|(model, all)| row(model, &all))
        .collect();
    rows.sort_by(|a, b| {
        let score = |row: &CouncilRow| row.peer_score.unwrap_or(f64::MIN);
        score(b)
            .total_cmp(&score(a))
            .then(b.answers.cmp(&a.answers))
            .then(a.model.cmp(&b.model))
    });
    rows
}

fn row(model: String, all: &[Scored]) -> CouncilRow {
    let scores: Vec<f64> = all.iter().filter_map(|scored| scored.peer_score).collect();
    CouncilRow {
        model,
        answers: count(all.len()),
        scored: count(scores.len()),
        peer_score: mean(&scores),
        best_answer: count(all.iter().filter(|scored| scored.best).count()),
    }
}

/// One answer of a council: its anonymous label, the member that gave it,
/// and the model that member answered with.
struct Answer<'a> {
    anon: String,
    member: &'a str,
    model: String,
}

/// Each answer of one council with what the other members made of it.
fn scored_answers(run: &Value) -> Vec<Scored> {
    let answers: Vec<Answer<'_>> = list(run, "answers")
        .iter()
        .filter_map(|answer| {
            let member = text(answer, "member")?;
            Some(Answer {
                anon: anon_label(text(answer, "anon_label")?),
                member,
                model: model_name(text(answer, "model_used"), member),
            })
        })
        .collect();
    let peer_scores: Vec<Option<f64>> = answers
        .iter()
        .map(|answer| mean(&scores_from_others(run, answer, &answers)))
        .collect();
    let top = peer_scores
        .iter()
        .flatten()
        .copied()
        .fold(f64::MIN, f64::max);
    let at_top = peer_scores
        .iter()
        .flatten()
        .filter(|score| **score == top)
        .count();
    let contested = peer_scores.iter().flatten().count() > 1;
    answers
        .into_iter()
        .zip(peer_scores)
        .map(|(answer, peer_score)| Scored {
            model: answer.model,
            peer_score,
            best: contested && at_top == 1 && peer_score == Some(top),
        })
        .collect()
}

/// The scores `answer` got from a different model than the one that wrote
/// it. A model's score of its own answer is its opinion of itself — also
/// when it sits on the council twice, or stood in for another member as that
/// member's fallback. A score that names no scorer cannot be told from one,
/// and is left out too. Two names for one model (`codex/default` and the
/// model that default resolves to) cannot be told apart from here.
fn scores_from_others(run: &Value, answer: &Answer<'_>, answers: &[Answer<'_>]) -> Vec<f64> {
    let model_of = |member: &str| {
        answers
            .iter()
            .find(|other| other.member == member)
            .map_or_else(|| model_name(None, member), |other| other.model.clone())
    };
    list(run, "scores")
        .iter()
        .filter(|given| {
            // A review can run on another model than the member's answer
            // did; where the run recorded which, that is the scorer.
            let scorer = text(given, "model_used")
                .map(String::from)
                .or_else(|| text(given, "member").map(model_of));
            scorer.is_some_and(|scorer| scorer != answer.model)
        })
        .flat_map(|given| list(given, "parsed"))
        .filter(|score| text(score, "agent").map(anon_label).as_deref() == Some(&answer.anon))
        .filter_map(|score| score.get("score")?.as_f64())
        .filter(|score| (0.0..=TOP_SCORE).contains(score))
        .collect()
}

/// An answer's anonymous label as written: "A", "a" and "Agent A" are one.
fn anon_label(written: &str) -> String {
    written
        .split_whitespace()
        .next_back()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
