//! Who stands out — only where there is enough to go on.
//!
//! A nomination compares. With too few results behind a model (`MIN_SAMPLE`)
//! there is nothing to compare, and with only one model that has enough,
//! there is no one to compare it with: no nomination is made.

use super::{CouncilRow, Nomination, ReviewerRow, Standings, WorkerRow, MIN_SAMPLE};

/// How many models must have enough results before one of them can stand out.
const MIN_FIELD: usize = 2;

pub(super) fn of(standings: &Standings) -> Vec<Nomination> {
    [
        best_answers(&standings.council),
        sharpest_eye(&standings.reviewers),
        most_false_alarms(&standings.reviewers),
        cleanest_work(&standings.workers),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// `rows` when enough of them qualify for a comparison to mean something.
fn field<T>(rows: Vec<T>) -> Vec<T> {
    if rows.len() < MIN_FIELD {
        Vec::new()
    } else {
        rows
    }
}

fn best_answers(rows: &[CouncilRow]) -> Option<Nomination> {
    let scored: Vec<(&CouncilRow, f64)> = rows
        .iter()
        .filter(|row| row.scored >= MIN_SAMPLE)
        .filter_map(|row| Some((row, row.peer_score?)))
        .collect();
    field(scored)
        .into_iter()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(row, score)| Nomination {
            title: "Best answers".to_string(),
            model: row.model.clone(),
            because: format!(
                "{score:.1}/10 from the other members over {} answers",
                row.scored
            ),
        })
}

/// Findings a second model ruled one way or the other.
fn ruled(row: &ReviewerRow) -> u32 {
    row.confirmed + row.refuted
}

/// Reviewers with enough ruled findings to be compared.
fn examined(rows: &[ReviewerRow]) -> Vec<&ReviewerRow> {
    field(rows.iter().filter(|row| ruled(row) >= MIN_SAMPLE).collect())
}

fn sharpest_eye(rows: &[ReviewerRow]) -> Option<Nomination> {
    examined(rows)
        .into_iter()
        .filter(|row| row.confirmed * 2 >= ruled(row))
        .max_by_key(|row| (row.confirmed, std::cmp::Reverse(row.refuted)))
        .map(|row| Nomination {
            title: "Sharpest eye".to_string(),
            model: row.model.clone(),
            because: format!(
                "{} of {} findings confirmed by a second model",
                row.confirmed,
                ruled(row)
            ),
        })
}

fn most_false_alarms(rows: &[ReviewerRow]) -> Option<Nomination> {
    examined(rows)
        .into_iter()
        .filter(|row| row.refuted * 2 > ruled(row))
        .max_by(|a, b| share(a.refuted, ruled(a)).total_cmp(&share(b.refuted, ruled(b))))
        .map(|row| Nomination {
            title: "Most false alarms".to_string(),
            model: row.model.clone(),
            because: format!(
                "{} of {} findings refuted by a second model",
                row.refuted,
                ruled(row)
            ),
        })
}

fn cleanest_work(rows: &[WorkerRow]) -> Option<Nomination> {
    let tried: Vec<&WorkerRow> = rows
        .iter()
        .filter(|row| row.subtasks >= MIN_SAMPLE)
        .collect();
    field(tried)
        .into_iter()
        .filter(|row| row.first_try * 2 >= row.subtasks)
        .max_by(|a, b| {
            share(a.first_try, a.subtasks)
                .total_cmp(&share(b.first_try, b.subtasks))
                .then(a.subtasks.cmp(&b.subtasks))
        })
        .map(|row| Nomination {
            title: "Cleanest work".to_string(),
            model: row.model.clone(),
            because: format!(
                "{} of {} subtasks accepted first try",
                row.first_try, row.subtasks
            ),
        })
}

fn share(part: u32, whole: u32) -> f64 {
    f64::from(part) / f64::from(whole.max(1))
}
