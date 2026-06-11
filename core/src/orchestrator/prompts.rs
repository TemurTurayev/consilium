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

pub fn diff_review(diff: &str) -> String {
    format!(
        "Review this diff for real problems: bugs, security issues, broken edge \
         cases, misleading naming. Do not invent style nitpicks. Then output EXACTLY \
         one JSON code block:\n```json\n{{\"findings\":[{{\"severity\":\"critical|important|minor\",\"file\":\"path\",\"description\":\"...\"}}]}}\n```\n\
         Empty findings array means the diff is clean.\n\nDiff:\n```diff\n{diff}\n```"
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
