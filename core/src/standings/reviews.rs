//! Review findings and the rulings a second model gave on them.

use super::names::{is_kind, list, text};
use super::{ExaminerRow, ReviewerRow};
use serde_json::Value;
use std::collections::BTreeMap;

/// One review as a run recorded it: who reviewed, who ruled, and the rulings.
struct Review<'a> {
    reviewer: &'a str,
    examiner: Option<&'a str>,
    /// The examiner's family neither wrote the diff nor reviewed it.
    independent: bool,
    rulings: Vec<&'a str>,
}

/// How rulings fell: (confirmed, refuted, unclear).
#[derive(Default, Clone, Copy)]
struct Rulings {
    confirmed: u32,
    refuted: u32,
    unclear: u32,
}

impl Rulings {
    fn of(rulings: &[&str]) -> Self {
        let count = |word: &str| {
            u32::try_from(rulings.iter().filter(|ruling| **ruling == word).count())
                .unwrap_or(u32::MAX)
        };
        Self {
            confirmed: count("confirmed"),
            refuted: count("refuted"),
            unclear: count("unclear"),
        }
    }

    fn plus(self, other: Self) -> Self {
        Self {
            confirmed: self.confirmed + other.confirmed,
            refuted: self.refuted + other.refuted,
            unclear: self.unclear + other.unclear,
        }
    }
}

pub(super) fn reviewer_rows(runs: &[Value]) -> Vec<ReviewerRow> {
    let reviews = all_reviews(runs);
    // A reviewer's record rests on rulings by a model with no stake; any
    // other ruling says too little about the reviewer to count either way.
    let counted = reviews.iter().map(|review| {
        let rulings = if review.independent {
            Rulings::of(&review.rulings)
        } else {
            Rulings::default()
        };
        (review.reviewer, rulings)
    });
    let mut rows: Vec<ReviewerRow> = grouped(counted)
        .into_iter()
        .map(|(model, (reviews, rulings))| {
            let ruled = rulings.confirmed + rulings.refuted;
            ReviewerRow {
                model,
                reviews,
                confirmed: rulings.confirmed,
                refuted: rulings.refuted,
                unclear: rulings.unclear,
                held_up: (ruled > 0).then(|| f64::from(rulings.confirmed) / f64::from(ruled)),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.confirmed
            .cmp(&a.confirmed)
            .then(b.reviews.cmp(&a.reviews))
            .then(a.model.cmp(&b.model))
    });
    rows
}

pub(super) fn examiner_rows(runs: &[Value]) -> Vec<ExaminerRow> {
    let reviews = all_reviews(runs);
    let examined = reviews
        .iter()
        .filter_map(|review| Some((review.examiner?, Rulings::of(&review.rulings))));
    let mut rows: Vec<ExaminerRow> = grouped(examined)
        .into_iter()
        .map(|(model, (examinations, rulings))| ExaminerRow {
            model,
            examinations,
            confirmed: rulings.confirmed,
            refuted: rulings.refuted,
            unclear: rulings.unclear,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.examinations
            .cmp(&a.examinations)
            .then(a.model.cmp(&b.model))
    });
    rows
}

/// Reviews and the rulings counted for each, added up per model.
fn grouped<'a>(
    reviews: impl Iterator<Item = (&'a str, Rulings)>,
) -> BTreeMap<String, (u32, Rulings)> {
    reviews.fold(BTreeMap::new(), |mut by_model, (model, rulings)| {
        let (count, so_far) = by_model
            .get(model)
            .copied()
            .unwrap_or((0, Rulings::default()));
        by_model.insert(model.to_string(), (count + 1, so_far.plus(rulings)));
        by_model
    })
}

/// Every review the runs hold: a `review` run is one, and so is each
/// attempt of a `conduct` run that its review gate looked at.
fn all_reviews(runs: &[Value]) -> Vec<Review<'_>> {
    let standalone = runs
        .iter()
        .filter(|run| is_kind(run, "review"))
        .filter_map(|run| review(run, "model_used"));
    let gated = runs
        .iter()
        .filter(|run| is_kind(run, "conduct"))
        .flat_map(|run| list(run, "subtasks"))
        .flat_map(|subtask| list(subtask, "attempts"))
        .filter_map(|attempt| review(attempt, "reviewer_model"));
    standalone.chain(gated).collect()
}

fn review<'a>(record: &'a Value, reviewer_key: &str) -> Option<Review<'a>> {
    Some(Review {
        reviewer: text(record, reviewer_key)?,
        examiner: text(record, "examined_by"),
        independent: text(record, "examiner_independence") == Some("independent"),
        rulings: list(record, "findings")
            .iter()
            .filter_map(|finding| text(finding, "ruling"))
            .collect(),
    })
}
