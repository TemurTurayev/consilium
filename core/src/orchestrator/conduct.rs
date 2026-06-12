//! Conduct contracts: structs, parsers, and test suite.
//! Orchestration logic (run_conduct) lands in Task 6.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Subtask {
    pub id: u32,
    #[serde(default)]
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub depends_note: String,
}

#[derive(Debug, Deserialize)]
pub struct Plan {
    pub subtasks: Vec<Subtask>,
}

pub fn parse_plan(text: &str) -> Option<Plan> {
    super::json_extract::extract_json_object::<Plan>(text)
}

#[derive(Debug, PartialEq)]
pub enum EvalDecision {
    Accept,
    Rework,
    Fail,
}

#[derive(Debug, Deserialize)]
pub struct Evaluation {
    #[serde(deserialize_with = "lenient_decision", default = "default_decision")]
    pub decision: EvalDecision,
    #[serde(default)]
    pub feedback: String,
}

fn default_decision() -> EvalDecision {
    EvalDecision::Rework
}

// Fail-safe: anything unrecognized becomes Rework — never silent acceptance.
fn lenient_decision<'de, D: serde::Deserializer<'de>>(d: D) -> Result<EvalDecision, D::Error> {
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "accept" => EvalDecision::Accept,
        "fail" => EvalDecision::Fail,
        _ => EvalDecision::Rework,
    })
}

pub fn parse_evaluation(text: &str) -> Option<Evaluation> {
    super::json_extract::extract_json_object::<Evaluation>(text)
}

#[derive(Debug, PartialEq)]
pub enum SupervisorStatus {
    Ok,
    Concern,
    Halt,
}

#[derive(Debug, Deserialize)]
pub struct SupervisorVerdict {
    #[serde(deserialize_with = "lenient_status", default = "default_status")]
    pub status: SupervisorStatus,
    #[serde(default)]
    pub note: String,
}

fn default_status() -> SupervisorStatus {
    SupervisorStatus::Concern
}

// Fail-safe: unknown status is a Concern (logged, surfaced), never silent Ok.
fn lenient_status<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SupervisorStatus, D::Error> {
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "ok" => SupervisorStatus::Ok,
        "halt" => SupervisorStatus::Halt,
        _ => SupervisorStatus::Concern,
    })
}

pub fn parse_supervisor(text: &str) -> Option<SupervisorVerdict> {
    super::json_extract::extract_json_object::<SupervisorVerdict>(text)
}

#[derive(Debug, Deserialize)]
pub struct Triage {
    #[serde(default)]
    complexity: String,
}

impl Triage {
    /// Fail-safe: unknown complexity → standard (full pipeline, never skipped).
    pub fn is_trivial(&self) -> bool {
        self.complexity.trim().eq_ignore_ascii_case("trivial")
    }
}

pub fn parse_triage(text: &str) -> Option<Triage> {
    super::json_extract::extract_json_object::<Triage>(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan() {
        let text = r#"```json
{"subtasks":[{"id":1,"title":"add module","prompt":"Create src/x.rs with ...","depends_note":""}]}
```"#;
        let plan = parse_plan(text).unwrap();
        assert_eq!(plan.subtasks.len(), 1);
        assert_eq!(plan.subtasks[0].id, 1);
    }

    #[test]
    fn parses_evaluation_variants() {
        for (s, expected) in [
            (
                r#"{"decision":"accept","feedback":""}"#,
                EvalDecision::Accept,
            ),
            (
                r#"{"decision":"rework","feedback":"missing tests"}"#,
                EvalDecision::Rework,
            ),
            (
                r#"{"decision":"fail","feedback":"impossible"}"#,
                EvalDecision::Fail,
            ),
        ] {
            assert_eq!(parse_evaluation(s).unwrap().decision, expected);
        }
    }

    #[test]
    fn unknown_decision_maps_to_rework() {
        // Fail-safe: an unrecognized decision must not auto-accept.
        let v = parse_evaluation(r#"{"decision":"lgtm!","feedback":"x"}"#).unwrap();
        assert_eq!(v.decision, EvalDecision::Rework);
    }

    #[test]
    fn parses_supervisor_verdict() {
        let v = parse_supervisor(r#"{"status":"halt","note":"scope creep"}"#).unwrap();
        assert_eq!(v.status, SupervisorStatus::Halt);
    }

    #[test]
    fn unknown_supervisor_status_maps_to_concern() {
        let v = parse_supervisor(r#"{"status":"hmm","note":""}"#).unwrap();
        assert_eq!(v.status, SupervisorStatus::Concern);
    }

    #[test]
    fn parses_triage() {
        assert!(parse_triage(r#"{"complexity":"trivial"}"#)
            .unwrap()
            .is_trivial());
        assert!(!parse_triage(r#"{"complexity":"standard"}"#)
            .unwrap()
            .is_trivial());
        assert!(!parse_triage(r#"{"complexity":"weird"}"#)
            .unwrap()
            .is_trivial()); // fail-safe: unknown → standard
    }

    #[test]
    fn decompose_template_example_parses_as_plan() {
        let p = crate::orchestrator::prompts::conduct_decompose("t", "ctx");
        assert!(parse_plan(&p).is_some());
    }

    #[test]
    fn evaluation_template_example_parses() {
        let p = crate::orchestrator::prompts::conduct_evaluation("t", "diff", "report", None);
        assert!(parse_evaluation(&p).is_some());
    }

    #[test]
    fn supervisor_template_example_parses() {
        let p = crate::orchestrator::prompts::supervisor_gate("task", "progress");
        assert!(parse_supervisor(&p).is_some());
    }

    #[test]
    fn triage_template_example_parses() {
        let p = crate::orchestrator::prompts::auto_triage("task");
        assert!(parse_triage(&p).is_some());
    }
}
