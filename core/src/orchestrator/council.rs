use super::prompts;
use super::runner::{run_to_completion, RunStatus};
use crate::adapters::{Adapter, RunRequest};
use crate::quota::QuotaStore;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub struct CouncilMember {
    pub label: String,
    pub adapter: Arc<dyn Adapter>,
    /// Model passed to the CLI (`--model`/`-m`); None = CLI default.
    pub model: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Score {
    pub agent: String,
    pub score: u8,
    #[serde(default)]
    pub justification: String,
}

#[derive(Debug, Deserialize)]
struct ScoresEnvelope {
    scores: Vec<Score>,
}

/// Extracts a `{"scores":[...]}` JSON object from model output via the shared
/// lenient extractor (fenced ```json block preferred, then a bounded scan).
/// Returns None on any parse failure — councils tolerate sloppy reviewers.
pub fn parse_scores(text: &str) -> Option<Vec<Score>> {
    super::json_extract::extract_json_object::<ScoresEnvelope>(text).map(|e| e.scores)
}

#[derive(Debug)]
pub struct CouncilOutcome {
    pub synthesis: String,
    /// (anonymized label, member label, answer text)
    pub answers: Vec<(String, String, String)>,
    pub failed_members: Vec<String>,
    pub transcript: serde_json::Value,
}

pub async fn run_council(
    question: &str,
    members: Vec<CouncilMember>,
    chairman: Arc<dyn Adapter>,
    chairman_model: Option<String>,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
) -> anyhow::Result<CouncilOutcome> {
    // Stage 1: independent answers, in parallel.
    let answer_prompt = prompts::council_answer(question);
    // Collect owned tuples to avoid borrow-checker issues with parallel futures.
    let member_data: Vec<(Arc<dyn Adapter>, Option<String>, String)> = members
        .iter()
        .map(|m| (m.adapter.clone(), m.model.clone(), m.label.clone()))
        .collect();
    let futures: Vec<_> = member_data
        .iter()
        .map(|(adapter, model, _label)| {
            let req = RunRequest {
                prompt: answer_prompt.clone(),
                model: model.clone(),
                cwd: cwd.clone(),
            };
            run_to_completion(adapter.clone(), req, quota, timeout)
        })
        .collect();
    let results = futures::future::join_all(futures).await;

    let mut answers: Vec<(String, String, String)> = Vec::new(); // (anon label, member label, text)
    let mut failed_members: Vec<String> = Vec::new();
    for ((_, _, label), result) in member_data.iter().zip(results) {
        match result {
            Ok(outcome)
                if matches!(outcome.status, RunStatus::Completed)
                    && !outcome.final_text.is_empty() =>
            {
                answers.push((String::new(), label.clone(), outcome.final_text));
            }
            _ => failed_members.push(label.clone()),
        }
    }
    if answers.is_empty() {
        anyhow::bail!("no council member produced an answer");
    }

    // Anonymize: assign labels A, B, C... in shuffled order.
    use rand::seq::SliceRandom;
    let mut order: Vec<usize> = (0..answers.len()).collect();
    order.shuffle(&mut rand::thread_rng());
    for (anon_idx, original_idx) in order.iter().enumerate() {
        answers[*original_idx].0 = char::from(b'A' + anon_idx as u8).to_string();
    }
    answers.sort_by(|a, b| a.0.cmp(&b.0));

    // Stage 2: each surviving member reviews the anonymized set, in parallel.
    let anon_pairs: Vec<(&str, &str)> = answers
        .iter()
        .map(|(label, _, text)| (label.as_str(), text.as_str()))
        .collect();
    let review_prompt = prompts::council_review(question, &anon_pairs);
    // Collect surviving member data (owned) to avoid borrow issues.
    let surviving: Vec<(Arc<dyn Adapter>, Option<String>, String)> = members
        .iter()
        .filter(|m| !failed_members.contains(&m.label))
        .map(|m| (m.adapter.clone(), m.model.clone(), m.label.clone()))
        .collect();
    let review_futures: Vec<_> = surviving
        .iter()
        .map(|(adapter, model, _label)| {
            let req = RunRequest {
                prompt: review_prompt.clone(),
                model: model.clone(),
                cwd: cwd.clone(),
            };
            run_to_completion(adapter.clone(), req, quota, timeout)
        })
        .collect();
    let review_results = futures::future::join_all(review_futures).await;

    let mut reviews: Vec<String> = Vec::new();
    let mut scores: Vec<(String, Option<Vec<Score>>)> = Vec::new();
    for ((_, _, label), result) in surviving.iter().zip(review_results) {
        if let Ok(outcome) = result {
            if matches!(outcome.status, RunStatus::Completed) {
                scores.push((label.clone(), parse_scores(&outcome.final_text)));
                reviews.push(outcome.final_text);
            }
        }
    }

    // Stage 3: chairman synthesis.
    let review_refs: Vec<&str> = reviews.iter().map(String::as_str).collect();
    let synthesis_prompt = prompts::council_synthesis(question, &anon_pairs, &review_refs);
    let synthesis_outcome = run_to_completion(
        chairman,
        RunRequest {
            prompt: synthesis_prompt,
            model: chairman_model,
            cwd,
        },
        quota,
        timeout,
    )
    .await?;
    if !matches!(synthesis_outcome.status, RunStatus::Completed) {
        anyhow::bail!(
            "chairman failed to synthesize: {:?}",
            synthesis_outcome.status
        );
    }

    let transcript = serde_json::json!({
        "kind": "council",
        "question": question,
        "answers": answers.iter().map(|(anon, member, text)| serde_json::json!({
            "anon_label": anon, "member": member, "text": text
        })).collect::<Vec<_>>(),
        "failed_members": failed_members,
        "reviews": reviews,
        "scores": scores.iter().map(|(member, s)| serde_json::json!({
            "member": member,
            "parsed": s.as_ref().map(|v| v.iter().map(|sc| serde_json::json!({
                "agent": sc.agent, "score": sc.score, "justification": sc.justification
            })).collect::<Vec<_>>()),
        })).collect::<Vec<_>>(),
        "synthesis": synthesis_outcome.final_text,
    });

    Ok(CouncilOutcome {
        synthesis: synthesis_outcome.final_text,
        answers,
        failed_members,
        transcript,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_scores_block() {
        let text = "Thoughts...\n```json\n{\"scores\":[{\"agent\":\"A\",\"score\":8,\"justification\":\"solid\"}]}\n```\ndone";
        let scores = parse_scores(text).unwrap();
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].agent, "A");
        assert_eq!(scores[0].score, 8);
    }

    #[test]
    fn parses_bare_json_without_fence() {
        let text = r#"{"scores":[{"agent":"B","score":3,"justification":"weak"}]}"#;
        let scores = parse_scores(text).unwrap();
        assert_eq!(scores[0].agent, "B");
    }

    #[test]
    fn malformed_output_yields_none() {
        assert!(parse_scores("no json here at all").is_none());
        assert!(parse_scores("```json\n{\"broken\": tru\n```").is_none());
    }

    #[test]
    fn parses_bare_json_with_trailing_prose() {
        let text =
            r#"{"scores":[{"agent":"A","score":6,"justification":"meh"}]} and that's my review."#;
        assert_eq!(parse_scores(text).unwrap()[0].score, 6);
    }

    #[test]
    fn parses_json_after_stray_braces() {
        let text = r#"I think {agent A} wins. {"scores":[{"agent":"A","score":9,"justification":"strong"}]}"#;
        assert_eq!(parse_scores(text).unwrap()[0].score, 9);
    }

    /// Guard against drift between the prompt template's embedded JSON example
    /// and the parser's schema: the example inside council_review's output
    /// must parse via parse_scores.
    #[test]
    fn template_json_example_parses_as_score() {
        let prompt = crate::orchestrator::prompts::council_review("q", &[("A", "ans")]);
        let scores = parse_scores(&prompt).expect("template example must parse");
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].agent, "A");
        assert_eq!(scores[0].score, 8);
    }
}
