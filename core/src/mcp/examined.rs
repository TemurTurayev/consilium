//! A review after cross-examination, as `review_diff` reports and saves it.

use super::types::FindingOut;
use crate::orchestrator::cross_examine::{findings_record, CrossExamined};
use crate::orchestrator::review::Finding;
use crate::text::one_line;
use serde_json::{json, Map, Value};

/// How much of an examiner's failure the tool result quotes.
const WHY_CHARS: usize = 240;

/// What became of a review's cross-examination.
pub(super) enum Examination {
    Examined(CrossExamined),
    /// The review has no critical or important finding.
    NothingToExamine,
    /// The caller asked for none.
    Off,
    /// No other model could rule; the findings stand as written.
    Unavailable(String),
}

impl Examination {
    /// The one-word account the tool result gives.
    pub(super) fn status(&self) -> String {
        match self {
            Examination::Examined(_) => "examined".to_string(),
            Examination::NothingToExamine => "nothing_to_examine".to_string(),
            Examination::Off => "off".to_string(),
            Examination::Unavailable(why) => {
                format!("unavailable: {}", one_line(why, WHY_CHARS))
            }
        }
    }

    pub(super) fn examined(&self) -> Option<&CrossExamined> {
        match self {
            Examination::Examined(examined) => Some(examined),
            _ => None,
        }
    }
}

/// The findings as the tool reports them, each with the ruling on it.
pub(super) fn findings_out(
    findings: &[Finding],
    examined: Option<&CrossExamined>,
) -> Vec<FindingOut> {
    findings
        .iter()
        .enumerate()
        .map(|(index, finding)| {
            let ruled = examined.and_then(|examined| examined.on(index));
            FindingOut {
                severity: finding.severity.as_str().to_string(),
                file: finding.file.clone(),
                description: finding.description.clone(),
                ruling: ruled.map(|(ruled, _)| ruled.ruling.as_str().to_string()),
                ruling_reason: ruled.map(|(ruled, _)| ruled.reason.clone()),
            }
        })
        .collect()
}

/// Who wrote the diff and how the reviewer was chosen — what the review's
/// own transcript does not know.
pub(super) struct Circumstances<'a> {
    pub author_model: Option<&'a str>,
    pub cross_family: &'a str,
    /// Every model passed over for the review or the rulings, and why.
    pub fallbacks: &'a [String],
    /// Whether the diff's author could be placed. If not, no examiner can be
    /// shown to be independent of it.
    pub author_known: bool,
}

/// Where the examiner stands, as the tool reports it.
pub(super) fn standing(examined: &CrossExamined, author_known: bool) -> &'static str {
    if author_known {
        examined.independence.as_str()
    } else {
        "unknown_author"
    }
}

/// The review's transcript with its circumstances, each finding and each
/// ruling added: the record `crate::standings` counts from.
pub(super) fn record(
    transcript: &Value,
    findings: &[Finding],
    circumstances: &Circumstances<'_>,
    examination: &Examination,
) -> Value {
    let examined = examination.examined();
    let added = [
        ("author_model", json!(circumstances.author_model)),
        ("cross_family", json!(circumstances.cross_family)),
        ("fallbacks", json!(circumstances.fallbacks)),
        ("findings", findings_record(findings, examined)),
        ("cross_examination", json!(examination.status())),
        ("examined_by", json!(examined.map(|e| e.examiner.as_str()))),
        (
            "examiner_independence",
            json!(examined.map(|e| standing(e, circumstances.author_known))),
        ),
    ];
    let merged: Map<String, Value> = transcript
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .chain(
            added
                .into_iter()
                .map(|(key, value)| (key.to_string(), value)),
        )
        .collect();
    Value::Object(merged)
}
