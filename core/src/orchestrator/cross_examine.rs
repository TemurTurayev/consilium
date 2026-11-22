//! Cross-examination: a second model rules on a reviewer's findings.
//!
//! A reviewer told to hunt for problems will also report some that are not
//! there. Acting on a false alarm costs a rework cycle; dismissing a real
//! problem costs more. So neither the reviewer nor the author gets the last
//! word: a model of another family checks each finding worth acting on against
//! the diff and rules it confirmed, refuted or unclear. The rulings are saved
//! with the review, which is what lets `crate::standings` say how often each
//! model's findings hold up — a number no model can raise by reporting more.

use super::council::CouncilMember;
use super::cross_family::examiner_ladder;
pub use super::cross_family::Independence;
use super::prompts;
use super::resilience::{run_with_failover, Fallback, ModelHealth, Rung};
use super::review::{Finding, Severity};
use super::runner::RunStatus;
use crate::adapters::RunRequest;
use crate::family::ModelFamily;
use crate::quota::QuotaStore;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

/// What the examiner decided about one finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ruling {
    /// The examiner can point at what makes the finding true.
    Confirmed,
    /// The diff or the code around it shows the finding is wrong.
    Refuted,
    /// Not settled — also what a finding gets when the examiner skipped it.
    #[default]
    Unclear,
}

impl Ruling {
    pub fn as_str(self) -> &'static str {
        match self {
            Ruling::Confirmed => "confirmed",
            Ruling::Refuted => "refuted",
            Ruling::Unclear => "unclear",
        }
    }

    /// Reads a ruling as a model wrote it. Anything unrecognised is unclear:
    /// a finding is never confirmed or dismissed on a word nobody defined.
    fn from_word(word: &str) -> Self {
        match word.trim().to_ascii_lowercase().as_str() {
            "confirmed" => Ruling::Confirmed,
            "refuted" => Ruling::Refuted,
            _ => Ruling::Unclear,
        }
    }
}

/// One finding's ruling and the examiner's reason for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Examined {
    pub ruling: Ruling,
    pub reason: String,
}

/// The reason recorded for a finding the examiner's reply did not rule on.
pub const NO_RULING: &str = "the examiner gave no ruling on this finding";

/// Whether a finding is examined at all. Minor ones are not: nothing is
/// gated on them, and the caller can judge them at a glance.
pub fn worth_examining(finding: &Finding) -> bool {
    !matches!(finding.severity, Severity::Minor)
}

/// Whether a finding still blocks the change after cross-examination. A
/// critical one does — unless a model with no stake refuted it. No one else's
/// refutation counts: the author's side is inclined to defend the diff, and
/// the reviewer's other rung is often the reviewer itself under another name
/// (`codex/default`).
pub fn blocks(finding: &Finding, examined: Option<(&Examined, Independence)>) -> bool {
    if !matches!(finding.severity, Severity::Critical) {
        return false;
    }
    !matches!(
        examined,
        Some((
            Examined {
                ruling: Ruling::Refuted,
                ..
            },
            Independence::Independent
        ))
    )
}

/// Whether any of a review's findings still blocks the change.
pub fn any_blocks(findings: &[Finding], examined: Option<&CrossExamined>) -> bool {
    findings
        .iter()
        .enumerate()
        .any(|(index, finding)| blocks(finding, examined.and_then(|e| e.on(index))))
}

/// A review's findings for a run's record: each with its ruling and the
/// reason for it, where it was examined.
pub fn findings_record(
    findings: &[Finding],
    examined: Option<&CrossExamined>,
) -> serde_json::Value {
    let listed: Vec<serde_json::Value> = findings
        .iter()
        .enumerate()
        .map(|(index, finding)| {
            let ruled = examined.and_then(|e| e.on(index)).map(|(ruled, _)| ruled);
            serde_json::json!({
                "severity": finding.severity.as_str(),
                "file": finding.file,
                "description": finding.description,
                "ruling": ruled.map(|ruled| ruled.ruling.as_str()),
                "reason": ruled.map(|ruled| ruled.reason.as_str()),
            })
        })
        .collect();
    serde_json::Value::Array(listed)
}

/// What a rework is asked to address, in the shape the reviewer reports
/// findings in: every finding that still blocks, plus those a second model
/// confirmed. A refuted finding, an unsettled non-critical one and a minor
/// one are not the worker's to chase. With no examination there is nothing
/// to tell them apart by, and all of them go, as before.
pub fn rework_findings(
    findings: &[Finding],
    examined: Option<&CrossExamined>,
) -> serde_json::Value {
    let handed: Vec<serde_json::Value> = findings
        .iter()
        .enumerate()
        .filter_map(|(index, finding)| {
            let ruled = examined.and_then(|e| e.on(index));
            let stated = serde_json::json!({
                "severity": finding.severity.as_str(),
                "file": finding.file,
                "description": finding.description,
            });
            match (examined, ruled) {
                (None, _) => Some(stated),
                (Some(_), Some((ruling, _)))
                    if blocks(finding, ruled) || ruling.ruling == Ruling::Confirmed =>
                {
                    Some(serde_json::json!({
                        "severity": finding.severity.as_str(),
                        "file": finding.file,
                        "description": finding.description,
                        "ruling": ruling.ruling.as_str(),
                        "reason": ruling.reason,
                    }))
                }
                (Some(_), None) if blocks(finding, None) => Some(stated),
                _ => None,
            }
        })
        .collect();
    serde_json::json!({ "findings": handed })
}

#[derive(Debug, Deserialize)]
struct RulingsEnvelope {
    rulings: Vec<RulingEntry>,
}

#[derive(Debug, Deserialize)]
struct RulingEntry {
    /// The finding's number, counted from 1 — models write it as 2 or "2".
    #[serde(default)]
    finding: serde_json::Value,
    #[serde(default)]
    ruling: String,
    #[serde(default)]
    reason: String,
}

/// Reads the examiner's reply into one ruling per finding, in the findings'
/// own order. `None` when the reply carries no rulings block at all.
pub fn parse_rulings(text: &str, findings: usize) -> Option<Vec<Examined>> {
    let envelope = super::json_extract::extract_json_object::<RulingsEnvelope>(text)?;
    let ruled: Vec<(usize, Examined)> = envelope
        .rulings
        .into_iter()
        .filter_map(|entry| Some((finding_number(&entry.finding)?, entry)))
        .map(|(number, entry)| {
            let examined = Examined {
                ruling: Ruling::from_word(&entry.ruling),
                reason: entry.reason.trim().to_string(),
            };
            (number, examined)
        })
        .collect();
    let ruling_on = |number: usize| {
        ruled
            .iter()
            .find(|(ruled_number, _)| *ruled_number == number)
            .map(|(_, examined)| examined.clone())
            .unwrap_or_else(|| Examined {
                ruling: Ruling::Unclear,
                reason: NO_RULING.to_string(),
            })
    };
    Some((1..=findings).map(ruling_on).collect())
}

/// A finding's number as a model wrote it: `2` or `"2"`.
fn finding_number(value: &serde_json::Value) -> Option<usize> {
    match value {
        serde_json::Value::Number(number) => usize::try_from(number.as_u64()?).ok(),
        serde_json::Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// The two sides of a review, and the models there are to rule between them.
pub struct Parties<'a> {
    /// The reviewer role's ladder.
    pub reviewer_ladder: &'a [Rung],
    /// The workers; each one's primary model can examine.
    pub workers: &'a [CouncilMember],
    /// The model that wrote the review ("provider/model"), when known.
    pub reviewer_model: Option<&'a str>,
    /// The family of the model that wrote the diff.
    pub author: ModelFamily,
    /// Ask only a model whose family neither wrote the diff nor reviewed it.
    /// For where the ruling decides something by itself (the review gate):
    /// no one else's refutation counts (see [`blocks`]), so asking them is a
    /// wasted call. Elsewhere any model but the reviewing one may rule, and
    /// the caller is told where it stands.
    pub independent_only: bool,
    /// What the diff was meant to do, when known. Without it the examiner
    /// cannot rule on a finding that says the diff does not do it.
    pub task: Option<&'a str>,
}

/// A review's findings after cross-examination.
#[derive(Debug)]
pub struct CrossExamined {
    /// One entry per finding of the review, in the review's order; `None`
    /// for a finding that was not examined (see [`worth_examining`]).
    pub rulings: Vec<Option<Examined>>,
    /// The model that ruled ("provider/model").
    pub examiner: String,
    /// Where that model stands between the reviewer and the author.
    pub independence: Independence,
    pub fallbacks: Vec<Fallback>,
}

impl CrossExamined {
    /// The same examination, judged against everyone who wrote part of the
    /// diff. A subtask's diff can hold the work of more than one model — a
    /// rework that fell through to another rung — and an examiner of any of
    /// their families is on the author's side, whoever touched it last.
    pub fn judged_against(self, authors: &[ModelFamily]) -> Self {
        let wrote_some =
            ModelFamily::of_author(&self.examiner).is_some_and(|family| authors.contains(&family));
        if wrote_some && self.independence == Independence::Independent {
            Self {
                independence: Independence::AuthorsFamily,
                ..self
            }
        } else {
            self
        }
    }

    /// The ruling on finding `index` (counted from 0) and its examiner's
    /// standing, as [`blocks`] takes them.
    pub fn on(&self, index: usize) -> Option<(&Examined, Independence)> {
        let examined = self.rulings.get(index)?.as_ref()?;
        Some((examined, self.independence))
    }
}

/// Has the most independent model the lineup offers rule on the findings of
/// a review that are worth acting on. Read-only. `None` when there is nothing
/// to examine — no model is called. The model that wrote the review is never
/// asked.
pub async fn examine_review(
    diff: &str,
    findings: &[Finding],
    parties: &Parties<'_>,
    health: &ModelHealth,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
) -> anyhow::Result<Option<CrossExamined>> {
    let chosen: Vec<(usize, &Finding)> = findings
        .iter()
        .enumerate()
        .filter(|(_, finding)| worth_examining(finding))
        .collect();
    if chosen.is_empty() {
        return Ok(None);
    }
    let reviewer = parties
        .reviewer_model
        .and_then(ModelFamily::of_author)
        .or_else(|| parties.reviewer_ladder.first().map(family_of))
        .ok_or_else(|| anyhow::anyhow!("no reviewer is known for these findings"))?;
    // Asking the model that wrote the findings whether they are right proves
    // nothing: it is left out, whatever else the lineup lacks.
    let (ranked, _) = examiner_ladder(
        parties.reviewer_ladder,
        parties.workers,
        reviewer,
        parties.author,
    );
    let may_rule = |rung: &Rung| {
        !parties.independent_only
            || Independence::of(family_of(rung), reviewer, parties.author)
                == Independence::Independent
    };
    let ladder: Vec<Rung> = ranked
        .into_iter()
        .filter(|rung| Some(model_key(rung).as_str()) != parties.reviewer_model)
        .filter(may_rule)
        .collect();
    if ladder.is_empty() {
        anyhow::bail!("no other model in the lineup to examine the findings");
    }
    let asked: Vec<&Finding> = chosen.iter().map(|(_, finding)| *finding).collect();
    let material = Material {
        diff,
        task: parties.task,
        findings: &asked,
    };
    let (ruled, examiner, fallbacks) =
        examine(&material, &ladder, health, quota, cwd, timeout).await?;
    let rulings = (0..findings.len())
        .map(|index| {
            let place = chosen
                .iter()
                .position(|(chosen_index, _)| *chosen_index == index)?;
            ruled.get(place).cloned()
        })
        .collect();
    // The standing is that of the model that ruled: a fallback can be of
    // another family than the one first in line.
    let independence = ModelFamily::of_author(&examiner)
        .map(|family| Independence::of(family, reviewer, parties.author))
        .unwrap_or(Independence::ReviewersFamily);
    Ok(Some(CrossExamined {
        rulings,
        examiner,
        independence,
        fallbacks,
    }))
}

/// What an examiner is given to rule on.
struct Material<'a> {
    diff: &'a str,
    task: Option<&'a str>,
    findings: &'a [&'a Finding],
}

/// The first available model on `ladder` rules on each of the findings.
async fn examine(
    material: &Material<'_>,
    ladder: &[Rung],
    health: &ModelHealth,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
) -> anyhow::Result<(Vec<Examined>, String, Vec<Fallback>)> {
    let findings = material.findings;
    let listed: Vec<(&str, &str, &str)> = findings
        .iter()
        .map(|f| (f.severity.as_str(), f.file.as_str(), f.description.as_str()))
        .collect();
    let prompt = prompts::cross_examine(material.diff, &listed, material.task);
    let fo = run_with_failover(
        ladder,
        "examiner",
        move |model| RunRequest {
            prompt: prompt.clone(),
            model,
            cwd: cwd.clone(),
            advisory: true, // rules on a diff — never changes a file
            write: false,
        },
        quota,
        health,
        timeout,
    )
    .await?;
    if !matches!(fo.outcome.status, RunStatus::Completed) {
        anyhow::bail!("examiner failed: {:?}", fo.outcome.status);
    }
    let rulings = parse_rulings(&fo.outcome.final_text, findings.len())
        .ok_or_else(|| anyhow::anyhow!("the examiner's reply carried no rulings"))?;
    Ok((rulings, fo.model_used, fo.fallbacks))
}

fn family_of(rung: &Rung) -> ModelFamily {
    ModelFamily::of(rung.candidate.provider, &rung.candidate.model)
}

/// "provider/model", as a run reports the model it used.
fn model_key(rung: &Rung) -> String {
    format!(
        "{}/{}",
        rung.candidate.provider.as_str(),
        rung.candidate.model
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn examined(ruling: Ruling, reason: &str) -> Examined {
        Examined {
            ruling,
            reason: reason.to_string(),
        }
    }

    #[test]
    fn reads_one_ruling_per_finding() {
        let reply = "Checked both.\n```json\n{\"rulings\":[\
            {\"finding\":1,\"ruling\":\"confirmed\",\"reason\":\"line 12 divides by len\"},\
            {\"finding\":2,\"ruling\":\"refuted\",\"reason\":\"guarded two lines above\"}]}\n```";
        assert_eq!(
            parse_rulings(reply, 2),
            Some(vec![
                examined(Ruling::Confirmed, "line 12 divides by len"),
                examined(Ruling::Refuted, "guarded two lines above"),
            ])
        );
    }

    #[test]
    fn rulings_follow_the_findings_order_not_the_replys() {
        let reply = r#"{"rulings":[{"finding":2,"ruling":"refuted","reason":"b"},{"finding":1,"ruling":"confirmed","reason":"a"}]}"#;
        assert_eq!(
            parse_rulings(reply, 2),
            Some(vec![
                examined(Ruling::Confirmed, "a"),
                examined(Ruling::Refuted, "b"),
            ])
        );
    }

    /// A finding the examiner skipped is neither confirmed nor dropped.
    #[test]
    fn a_finding_without_a_ruling_is_unclear() {
        let reply = r#"{"rulings":[{"finding":2,"ruling":"confirmed","reason":"real"}]}"#;
        assert_eq!(
            parse_rulings(reply, 2),
            Some(vec![
                examined(Ruling::Unclear, NO_RULING),
                examined(Ruling::Confirmed, "real"),
            ])
        );
    }

    #[test]
    fn a_ruling_in_unknown_words_is_unclear_and_keeps_its_reason() {
        let reply =
            r#"{"rulings":[{"finding":1,"ruling":"probably fine","reason":"hard to say"}]}"#;
        assert_eq!(
            parse_rulings(reply, 1),
            Some(vec![examined(Ruling::Unclear, "hard to say")])
        );
    }

    #[test]
    fn rulings_are_read_whatever_their_case_and_number_format() {
        let reply = r#"{"rulings":[{"finding":"1","ruling":" Confirmed ","reason":"x"},{"finding":2,"ruling":"REFUTED","reason":"y"}]}"#;
        assert_eq!(
            parse_rulings(reply, 2),
            Some(vec![
                examined(Ruling::Confirmed, "x"),
                examined(Ruling::Refuted, "y"),
            ])
        );
    }

    /// Numbers that name no finding are ignored, and a finding ruled on twice
    /// keeps its first ruling.
    #[test]
    fn rulings_on_findings_that_do_not_exist_are_ignored() {
        let reply = r#"{"rulings":[{"finding":0,"ruling":"refuted","reason":"zero"},{"finding":7,"ruling":"refuted","reason":"seven"},{"finding":1,"ruling":"confirmed","reason":"first"},{"finding":1,"ruling":"refuted","reason":"second"}]}"#;
        assert_eq!(
            parse_rulings(reply, 1),
            Some(vec![examined(Ruling::Confirmed, "first")])
        );
    }

    #[test]
    fn a_reply_without_rulings_is_no_examination() {
        assert_eq!(parse_rulings("I could not decide.", 2), None);
        assert_eq!(parse_rulings(r#"{"findings":[]}"#, 2), None);
    }

    /// Guards against drift between the format the prompt shows and the
    /// format this parser reads.
    #[test]
    fn the_prompts_own_example_parses() {
        let prompt = prompts::cross_examine("diff", &[("critical", "a.rs", "boom")], None);
        let rulings = parse_rulings(&prompt, 1).expect("the prompt's example must parse");
        assert_eq!(rulings.len(), 1);
    }

    #[test]
    fn a_ruling_is_named_as_the_prompt_names_it() {
        for ruling in [Ruling::Confirmed, Ruling::Refuted, Ruling::Unclear] {
            assert_eq!(Ruling::from_word(ruling.as_str()), ruling);
        }
    }

    fn finding(severity: &str) -> Finding {
        serde_json::from_value(serde_json::json!({
            "severity": severity, "file": "a.rs", "description": "d"
        }))
        .unwrap()
    }

    #[test]
    fn minor_findings_are_not_worth_a_model_call() {
        assert!(worth_examining(&finding("critical")));
        assert!(worth_examining(&finding("important")));
        assert!(!worth_examining(&finding("minor")));
    }

    #[test]
    fn only_a_critical_finding_blocks() {
        assert!(blocks(&finding("critical"), None));
        assert!(!blocks(&finding("important"), None));
        assert!(!blocks(&finding("minor"), None));
    }

    #[test]
    fn a_critical_finding_blocks_unless_it_is_refuted() {
        let critical = finding("critical");
        for ruling in [Ruling::Confirmed, Ruling::Unclear] {
            let ruled = examined(ruling, "r");
            assert!(
                blocks(&critical, Some((&ruled, Independence::Independent))),
                "{ruling:?}"
            );
        }
        let refuted = examined(Ruling::Refuted, "guarded above");
        assert!(!blocks(
            &critical,
            Some((&refuted, Independence::Independent))
        ));
    }

    /// A block is lifted only by a model with no stake: the author's side
    /// is inclined to defend the diff, and the reviewer's "other" model is
    /// often the reviewer itself under another name (`codex/default`).
    #[test]
    fn only_an_independent_refutation_lifts_a_block() {
        let critical = finding("critical");
        let refuted = examined(Ruling::Refuted, "looks fine to me");
        assert!(blocks(
            &critical,
            Some((&refuted, Independence::AuthorsFamily))
        ));
        assert!(blocks(
            &critical,
            Some((&refuted, Independence::ReviewersFamily))
        ));
        assert!(!blocks(
            &critical,
            Some((&refuted, Independence::Independent))
        ));
    }

    fn ruled(rulings: Vec<Option<Examined>>, independence: Independence) -> CrossExamined {
        CrossExamined {
            rulings,
            examiner: "gemini/gemini-x".to_string(),
            independence,
            fallbacks: Vec::new(),
        }
    }

    fn descriptions(findings: &serde_json::Value) -> Vec<&str> {
        findings["findings"]
            .as_array()
            .expect("a findings array")
            .iter()
            .filter_map(|finding| finding["description"].as_str())
            .collect()
    }

    fn described(severity: &str, description: &str) -> Finding {
        serde_json::from_value(serde_json::json!({
            "severity": severity, "file": "a.rs", "description": description
        }))
        .unwrap()
    }

    #[test]
    fn a_review_blocks_while_one_critical_finding_stands() {
        let findings = [finding("critical"), finding("critical")];
        let both_refuted = ruled(
            vec![
                Some(examined(Ruling::Refuted, "a")),
                Some(examined(Ruling::Refuted, "b")),
            ],
            Independence::Independent,
        );
        let one_stands = ruled(
            vec![
                Some(examined(Ruling::Refuted, "a")),
                Some(examined(Ruling::Unclear, "b")),
            ],
            Independence::Independent,
        );
        assert!(any_blocks(&findings, None));
        assert!(!any_blocks(&findings, Some(&both_refuted)));
        assert!(any_blocks(&findings, Some(&one_stands)));
        assert!(!any_blocks(&[finding("important")], None));
    }

    #[test]
    fn a_rework_is_handed_what_blocks_and_what_was_confirmed() {
        let findings = [
            described("critical", "unsettled critical"),
            described("important", "confirmed important"),
            described("important", "refuted important"),
            described("important", "unsettled important"),
            described("minor", "minor"),
        ];
        let examination = ruled(
            vec![
                Some(examined(Ruling::Unclear, "cannot tell")),
                Some(examined(Ruling::Confirmed, "seen on line 4")),
                Some(examined(Ruling::Refuted, "guarded")),
                Some(examined(Ruling::Unclear, "taste")),
                None,
            ],
            Independence::Independent,
        );
        let handed = rework_findings(&findings, Some(&examination));
        assert_eq!(
            descriptions(&handed),
            ["unsettled critical", "confirmed important"]
        );
        assert_eq!(handed["findings"][1]["ruling"], "confirmed");
        assert_eq!(handed["findings"][1]["reason"], "seen on line 4");
    }

    /// Nothing ruled on them, so nothing tells them apart: all go, as before.
    #[test]
    fn without_an_examination_a_rework_is_handed_every_finding() {
        let findings = [described("critical", "c"), described("minor", "m")];
        let handed = rework_findings(&findings, None);
        assert_eq!(descriptions(&handed), ["c", "m"]);
        assert_eq!(handed["findings"][0]["severity"], "critical");
        assert!(handed["findings"][0].get("ruling").is_none());
    }

    #[test]
    fn the_record_lists_every_finding_with_its_ruling() {
        let findings = [described("critical", "c"), described("minor", "m")];
        let examination = ruled(
            vec![Some(examined(Ruling::Refuted, "guarded")), None],
            Independence::Independent,
        );
        let record = findings_record(&findings, Some(&examination));
        assert_eq!(record[0]["severity"], "critical");
        assert_eq!(record[0]["file"], "a.rs");
        assert_eq!(record[0]["ruling"], "refuted");
        assert_eq!(record[0]["reason"], "guarded");
        assert_eq!(record[1]["description"], "m");
        assert!(record[1]["ruling"].is_null());
    }

    /// Gemini wrote the first attempt, Claude the rework: Gemini is not
    /// independent of that diff, although the last author was Claude.
    #[test]
    fn an_examiner_whose_family_wrote_an_earlier_attempt_has_a_stake() {
        let examination = || {
            ruled(
                vec![Some(examined(Ruling::Refuted, "r"))],
                Independence::Independent,
            )
        };

        let both = [ModelFamily::Google, ModelFamily::Anthropic];
        assert_eq!(
            examination().judged_against(&both).independence,
            Independence::AuthorsFamily
        );
        assert_eq!(
            examination()
                .judged_against(&[ModelFamily::Anthropic])
                .independence,
            Independence::Independent,
            "an examiner that wrote none of it keeps its standing"
        );
    }
}
