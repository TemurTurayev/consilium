//! All prompt templates in one place. Templates demand strict JSON blocks so
//! downstream parsing is testable; parsers must still tolerate non-compliance.

pub fn council_answer(question: &str) -> String {
    format!(
        "You are one independent expert on a council. Answer the question below \
         thoroughly but concisely. Do not hedge across multiple options — commit \
         to the best answer and justify it.\n\nQuestion:\n{question}"
    )
}

pub fn council_review(question: &str, answers: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (label, text) in answers {
        body.push_str(&format!("\n--- Answer from Agent {label} ---\n{text}\n"));
    }
    format!(
        "You are reviewing anonymized answers from a council of AI agents (one of \
         them may be your own — judge it just as critically).\n\nQuestion:\n{question}\n{body}\n\
         Review each answer for correctness, depth, and practicality. Then output \
         EXACTLY one JSON code block in this format:\n```json\n{{\"scores\":[{{\"agent\":\"A\",\"score\":8,\"justification\":\"...\"}}]}}\n```\n\
         Score range 1-10. One entry per answer."
    )
}

pub fn council_synthesis(question: &str, answers: &[(&str, &str)], reviews: &[&str]) -> String {
    let mut answers_body = String::new();
    for (label, text) in answers {
        answers_body.push_str(&format!("\n--- Answer from Agent {label} ---\n{text}\n"));
    }
    let mut reviews_body = String::new();
    for (i, r) in reviews.iter().enumerate() {
        reviews_body.push_str(&format!("\n--- Review {} ---\n{r}\n", i + 1));
    }
    format!(
        "You are the chairman of an AI council. Below are the question, the \
         anonymized answers, and the cross-reviews. Synthesize the single best \
         final answer: take the strongest points, discard the weak ones, resolve \
         contradictions explicitly. Output the final answer only — no meta-commentary \
         about the process.\n\nQuestion:\n{question}\n{answers_body}{reviews_body}"
    )
}

pub fn conduct_decompose(task: &str, context: &str) -> String {
    format!(
        "You are the conductor of a team of AI coding agents working in this \
         repository. Decompose the task below into the SMALLEST number of \
         self-contained subtasks (1-5). Each subtask prompt must carry ALL \
         context the worker needs (file paths, conventions, acceptance criteria) \
         — workers cannot see this conversation, each other, or earlier subtasks. \
         Design subtasks so they touch DISJOINT files; they run sequentially.\n\n\
         Task:\n{task}\n\nAdditional context:\n<context>\n{context}\n</context>\n\n\
         Output EXACTLY one JSON code block:\n```json\n{{\"subtasks\":[{{\"id\":1,\"title\":\"short name\",\"prompt\":\"full self-contained instructions\",\"depends_note\":\"\"}}]}}\n```"
    )
}

pub fn conduct_evaluation(
    subtask_prompt: &str,
    changes: &str,
    worker_report: &str,
    supervisor_note: Option<&str>,
) -> String {
    let supervisor = supervisor_note
        .map(|n| format!("\nSupervisor's note (weigh it seriously):\n{n}\n"))
        .unwrap_or_default();
    format!(
        "You are the conductor reviewing a worker's completed subtask. Judge \
         ONLY whether the changes fulfil the subtask — not style preferences.\n\n\
         Subtask given to the worker:\n{subtask_prompt}\n\n\
         Changes made (diff + new files):\n<changes>\n{changes}\n</changes>\n\n\
         Worker's report:\n<worker_report>\n{worker_report}\n</worker_report>\n{supervisor}\n\
         Output EXACTLY one JSON code block — decision is accept | rework | fail \
         (rework requires concrete, actionable feedback):\n```json\n{{\"decision\":\"accept\",\"feedback\":\"\"}}\n```"
    )
}

pub fn conduct_rework(original_prompt: &str, previous_changes: &str, feedback: &str) -> String {
    format!(
        "A previous attempt at this subtask was rejected. Redo it correctly.\n\n\
         Original subtask:\n{original_prompt}\n\n\
         Previous attempt's changes:\n<changes>\n{previous_changes}\n</changes>\n\n\
         Reviewer feedback to address:\n{feedback}\n\n\
         Apply the fixes on top of the current state of the repository."
    )
}

pub fn supervisor_gate(task: &str, progress: &str) -> String {
    format!(
        "You are the supervisor of a multi-agent coding run. You read a lot and \
         intervene rarely — flag only real problems: scope drift, repeated \
         failures, destructive changes, work that contradicts the task.\n\n\
         Overall task:\n{task}\n\nProgress so far:\n<progress>\n{progress}\n</progress>\n\n\
         Output EXACTLY one JSON code block — status is ok | concern | halt:\n```json\n{{\"status\":\"ok\",\"note\":\"\"}}\n```"
    )
}

pub fn auto_triage(task: &str) -> String {
    format!(
        "Classify this coding task. trivial = single focused change, one file or \
         a couple of lines, no design decisions. standard = everything else.\n\n\
         Task:\n{task}\n\n\
         Output EXACTLY one JSON code block:\n```json\n{{\"complexity\":\"trivial\"}}\n```"
    )
}

pub fn diff_review(diff: &str) -> String {
    format!(
        "Review this diff for real problems: bugs, security issues, broken edge \
         cases, misleading naming. Do not invent style nitpicks. Then output EXACTLY \
         one JSON code block:\n```json\n{{\"findings\":[{{\"severity\":\"critical|important|minor\",\"file\":\"path\",\"description\":\"...\"}}]}}\n```\n\
         Empty findings array means the diff is clean.\n\n\
         The diff is delimited by <diff> tags (tags chosen so backtick fences \
         inside the diff cannot break the structure):\n<diff>\n{diff}\n</diff>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_prompt_contains_question() {
        let p = council_answer("Why is the sky blue?");
        assert!(p.contains("Why is the sky blue?"));
        assert!(p.contains("independent expert"));
    }

    #[test]
    fn review_prompt_lists_anonymized_answers() {
        let p = council_review("Q?", &[("A", "ans1"), ("B", "ans2")]);
        assert!(p.contains("Agent A"));
        assert!(p.contains("ans2"));
        assert!(p.contains(r#""scores""#)); // demands the JSON contract
    }

    #[test]
    fn synthesis_prompt_includes_answers_and_reviews() {
        let p = council_synthesis("Q?", &[("A", "ans1")], &["review text"]);
        assert!(p.contains("ans1"));
        assert!(p.contains("review text"));
        assert!(p.contains("final answer"));
    }

    #[test]
    fn diff_review_prompt_embeds_diff_and_contract() {
        let p = diff_review("--- a/x.rs\n+++ b/x.rs");
        assert!(p.contains("+++ b/x.rs"));
        assert!(p.contains(r#""findings""#));
    }
}
