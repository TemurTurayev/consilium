use super::prompts;
use super::runner::{run_to_completion, RunStatus};
use crate::adapters::{Adapter, RunRequest};
use crate::quota::QuotaStore;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// Unknown severities map to Minor: tolerate creative reviewers.
#[derive(Debug, Default)]
pub enum Severity {
    Critical,
    Important,
    #[default]
    Minor,
}

#[derive(Debug, Deserialize)]
pub struct Finding {
    #[serde(default, deserialize_with = "lenient_severity")]
    pub severity: Severity,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub description: String,
}

fn lenient_severity<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Severity, D::Error> {
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "critical" => Severity::Critical,
        "important" => Severity::Important,
        _ => Severity::Minor,
    })
}

#[derive(Debug, Deserialize)]
pub struct Verdict {
    pub findings: Vec<Finding>,
}

impl Verdict {
    pub fn has_critical(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f.severity, Severity::Critical))
    }
}

/// Lenient extraction shared with council::parse_scores (json_extract).
pub fn parse_verdict(text: &str) -> Option<Verdict> {
    super::json_extract::extract_json_object::<Verdict>(text)
}

#[derive(Debug)]
pub struct ReviewResult {
    pub verdict: Option<Verdict>,
    pub raw_review: String,
    pub transcript: serde_json::Value,
}

pub async fn run_review(
    diff: &str,
    reviewer: Arc<dyn Adapter>,
    reviewer_model: Option<String>,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
) -> anyhow::Result<ReviewResult> {
    let prompt = prompts::diff_review(diff);
    let outcome = run_to_completion(
        reviewer,
        RunRequest {
            prompt,
            model: reviewer_model,
            cwd,
        },
        quota,
        timeout,
    )
    .await?;
    if !matches!(outcome.status, RunStatus::Completed) {
        anyhow::bail!("reviewer failed: {:?}", outcome.status);
    }
    let verdict = parse_verdict(&outcome.final_text);
    // First 32 KiB (by chars, so we never split a UTF-8 boundary) of the diff,
    // kept alongside diff_bytes so transcripts stay auditable without bloating.
    let diff_preview: String = diff.chars().take(32_768).collect();
    let transcript = serde_json::json!({
        "kind": "review",
        "diff_bytes": diff.len(),
        "diff_preview": diff_preview,
        "raw_review": outcome.final_text,
        "parse_ok": verdict.is_some(),
    });
    Ok(ReviewResult {
        verdict,
        raw_review: outcome.final_text,
        transcript,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_findings_block() {
        let text = "Analysis...\n```json\n{\"findings\":[{\"severity\":\"critical\",\"file\":\"a.rs\",\"description\":\"sql injection\"}]}\n```";
        let v = parse_verdict(text).unwrap();
        assert_eq!(v.findings.len(), 1);
        assert!(matches!(v.findings[0].severity, Severity::Critical));
    }

    #[test]
    fn empty_findings_means_clean() {
        let v = parse_verdict("```json\n{\"findings\":[]}\n```").unwrap();
        assert!(v.findings.is_empty());
        assert!(!v.has_critical());
    }

    #[test]
    fn malformed_verdict_yields_none() {
        assert!(parse_verdict("looks good to me!").is_none());
    }

    #[test]
    fn unknown_severity_maps_to_minor() {
        let v = parse_verdict("```json\n{\"findings\":[{\"severity\":\"catastrophic\",\"file\":\"x\",\"description\":\"d\"}]}\n```").unwrap();
        assert!(matches!(v.findings[0].severity, Severity::Minor));
    }

    /// Guard against drift between diff_review's embedded JSON example and the
    /// parser schema (carried from Task 4 review).
    #[test]
    fn template_json_example_parses_as_verdict() {
        let prompt = crate::orchestrator::prompts::diff_review("+x");
        let v = parse_verdict(&prompt).expect("template example must parse");
        assert_eq!(v.findings.len(), 1);
        // The template example severity is the literal "critical|important|minor",
        // which lenient_severity maps to Minor — asserting it parses is the point.
        assert!(matches!(v.findings[0].severity, Severity::Minor));
    }

    #[test]
    fn verdict_with_trailing_prose_parses() {
        let text = r#"{"findings":[]} overall looks fine to me"#;
        assert!(parse_verdict(text).unwrap().findings.is_empty());
    }

    #[test]
    fn capitalized_critical_still_gates() {
        let v = parse_verdict(
            "```json\n{\"findings\":[{\"severity\":\"Critical\",\"file\":\"a\",\"description\":\"d\"},{\"severity\":\"CRITICAL\",\"file\":\"b\",\"description\":\"d\"}]}\n```",
        )
        .unwrap();
        assert!(matches!(v.findings[0].severity, Severity::Critical));
        assert!(matches!(v.findings[1].severity, Severity::Critical));
        assert!(v.has_critical());
    }

    #[test]
    fn missing_severity_maps_to_minor() {
        let v =
            parse_verdict("```json\n{\"findings\":[{\"file\":\"a\",\"description\":\"d\"}]}\n```")
                .unwrap();
        assert!(matches!(v.findings[0].severity, Severity::Minor));
    }

    #[test]
    fn null_severity_maps_to_minor() {
        let v = parse_verdict(
            "```json\n{\"findings\":[{\"severity\":null,\"file\":\"a\",\"description\":\"d\"}]}\n```",
        )
        .unwrap();
        assert!(matches!(v.findings[0].severity, Severity::Minor));
    }

    #[test]
    fn sparse_finding_does_not_sink_sibling_critical() {
        let v = parse_verdict(
            "```json\n{\"findings\":[{\"severity\":\"critical\",\"file\":\"a\",\"description\":\"real\"},{\"file\":\"b\"}]}\n```",
        )
        .unwrap();
        assert_eq!(v.findings.len(), 2);
        assert!(v.has_critical());
        assert!(matches!(v.findings[1].severity, Severity::Minor));
        assert!(v.findings[1].description.is_empty());
    }

    /// Models often echo the prompt's embedded JSON example before the real
    /// verdict — the LAST parseable candidate must win, not the decoy.
    #[test]
    fn real_verdict_after_template_example_wins() {
        let text = format!(
            "{}\n```json\n{{\"findings\":[{{\"severity\":\"critical\",\"file\":\"x\",\"description\":\"real\"}}]}}\n```",
            crate::orchestrator::prompts::diff_review("+x")
        );
        let v = parse_verdict(&text).expect("real verdict must parse");
        assert!(v.has_critical());
        assert_eq!(v.findings[0].description, "real");
    }
}
