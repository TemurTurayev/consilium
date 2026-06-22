//! All prompt templates in one place. Templates demand strict JSON blocks so
//! downstream parsing is testable; parsers must still tolerate non-compliance.
//!
//! SECURITY (untrusted interpolation): repo/worker-authored content (`changes`,
//! `context`, `progress`) is XML-tag-isolated, but `feedback`, supervisor `note`,
//! and reviewer `findings` are interpolated bare. A hostile repo file or model
//! output could carry steering instructions into the conductor/supervisor/arbiter
//! prompts. Accepted for now: trusted local operator, user-initiated runs, CLI
//! write-scoping armed. TODO(M3): full prompt-injection hardening (delimit every
//! interpolation, instruct models to treat delimited spans as inert data).
//!
//! ConductorMemory blocks (`plan_ledger`, `attempt_history`) are conductor-
//! authored and mechanically bounded: the ledger summary is status + verify
//! digest only (no verbatim worker text); attempt_history carries the conductor's
//! own feedback. Both are XML-isolated like the trusted blocks above, so they do
//! not reintroduce the bare-interpolation hole.
//!
//! The worker blackboard (`prior_work`, on the INITIAL worker prompt) is a strict
//! subset shown to WORKERS: a mechanical roster of prior finished subtasks
//! (id/title/status) + files modified this run. Workers never see the conductor's
//! `plan_ledger` (verify digests) or any subtask's `feedback`/`attempt_history`
//! (the prose-bearing fields) — only this XML-isolated mechanical roster.

/// Render an optional XML-isolated context block. `None` → empty string, so a
/// prompt carrying no extra context stays byte-identical to its bare form.
fn memory_block(tag: &str, body: Option<&str>) -> String {
    body.map(|b| format!("\n<{tag}>\n{b}\n</{tag}>\n"))
        .unwrap_or_default()
}

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
         self-contained subtasks (1-5). Workers cannot see this conversation, each \
         other, or earlier subtasks, so each subtask `prompt` MUST RESTATE every \
         concrete constraint the worker needs: exact file paths, function/type \
         signatures, the required output or return shape, naming conventions, and \
         the specific edge cases and acceptance tests that define \"done\". A vague \
         prompt (\"implement X\") fails — name the artifact precisely. Design \
         subtasks so they touch DISJOINT files; they run sequentially.\n\n\
         Task:\n{task}\n\nAdditional context:\n<context>\n{context}\n</context>\n\n\
         Example of well-specified subtasks — note how each names exact signatures, \
         paths, and tests (this example is Rust; mirror the same precision in the \
         task's actual language/stack):\n\
         ```json\n{{\"subtasks\":[{{\"id\":1,\"title\":\"retry helper\",\"prompt\":\"In src/util/retry.rs add `pub async fn with_backoff<F,T>(max: u32, base: Duration, f: F) -> anyhow::Result<T>` that retries f up to max times, sleeping base*2^attempt between tries and returning the last error on exhaustion. Add #[tokio::test]s for success-after-one-failure and exhaustion. Touch only this file.\",\"depends_note\":\"\"}},{{\"id\":2,\"title\":\"wire --max-retries\",\"prompt\":\"In src/main.rs add a clap flag --max-retries <u32> (default 3) to the run subcommand and pass it into with_backoff; leave other flags unchanged.\",\"depends_note\":\"uses retry helper from subtask 1\"}}]}}\n```\n\n\
         Now output EXACTLY one JSON code block in the same shape for the task above:\n```json\n{{\"subtasks\":[{{\"id\":1,\"title\":\"short name\",\"prompt\":\"full self-contained instructions\",\"depends_note\":\"\"}}]}}\n```"
    )
}

pub fn conduct_replan(
    task: &str,
    context: &str,
    completed_summary: &str,
    failure_reason: &str,
) -> String {
    format!(
        "You are the conductor of a team of AI coding agents working in this \
         repository. Produce a REVISED plan for the task below covering ONLY \
         the work still needed after some subtasks already completed and the \
         run then hit a failure. Decompose the remaining work into the \
         SMALLEST number of self-contained subtasks (1-5). Each subtask prompt \
         must carry ALL context the worker needs (file paths, conventions, \
         acceptance criteria) — workers cannot see this conversation, each \
         other, or earlier subtasks. Do NOT redo already-completed work. \
         Number the new subtasks with fresh ids that continue AFTER the highest id \
         in the completed work above (never reuse a completed id). Design subtasks so \
         they touch DISJOINT files; they run sequentially.\n\n\
         Task:\n{task}\n\nAdditional context:\n<context>\n{context}\n</context>\n\n\
         Already completed work:\n<completed_summary>\n{completed_summary}\n</completed_summary>\n\n\
         Failure that requires replanning:\n<failure_reason>\n{failure_reason}\n</failure_reason>\n\n\
         Output EXACTLY one JSON code block:\n```json\n{{\"subtasks\":[{{\"id\":1,\"title\":\"short name\",\"prompt\":\"full self-contained instructions\",\"depends_note\":\"\"}}]}}\n```"
    )
}

/// Scope-discipline preamble prepended to every worker's initial prompt: bias the
/// worker toward the smallest correct change (the Ponytail-benchmark lesson)
/// without licensing corner-cutting.
const SCOPE_DISCIPLINE: &str = "Scope discipline — make the SMALLEST change that \
    fully satisfies the subtask: prefer the standard library or existing code over a \
    new dependency; add no abstraction, config, or files the subtask doesn't \
    require; do not refactor or reformat unrelated code. This is NOT license to cut \
    corners — keep error handling, edge cases, and the existing tests intact.";

/// Wraps a worker's INITIAL subtask prompt with the scope-discipline preamble and,
/// when present, the read-only `prior_work` blackboard (mechanical roster of prior
/// finished subtasks + files modified this run). Workers see only this mechanical
/// roster, never cross-subtask feedback or attempt history.
pub fn conduct_initial(subtask_prompt: &str, prior_work: Option<&str>) -> String {
    match prior_work {
        None => format!("{SCOPE_DISCIPLINE}\n\n{subtask_prompt}"),
        Some(pw) => format!(
            "{SCOPE_DISCIPLINE}\n\n{subtask_prompt}\n\n\
             Earlier subtasks in THIS run are already done — read-only context. Your \
             work must NOT overlap their files:\n<prior_work>\n{pw}\n</prior_work>"
        ),
    }
}

pub fn conduct_evaluation(
    subtask_prompt: &str,
    changes: &str,
    worker_report: &str,
    verify: &str,
    supervisor_note: Option<&str>,
    plan_ledger: Option<&str>,
    attempt_history: Option<&str>,
) -> String {
    let supervisor = supervisor_note
        .map(|n| format!("\nSupervisor's note (weigh it seriously):\n{n}\n"))
        .unwrap_or_default();
    let ledger = memory_block("plan_ledger", plan_ledger);
    let history = memory_block("attempt_history", attempt_history);
    format!(
        "You are the conductor reviewing a worker's completed subtask. Judge \
         whether the changes fulfil the subtask. Build/test results are AUTHORITATIVE: \
         if tests or build failed, you must NOT accept — request rework citing the \
         failure. If no verifier ran, treat your judgment as unverified and be \
         conservative.\n\n\
         Subtask given to the worker:\n{subtask_prompt}\n\n\
         Changes made (diff + new files):\n<changes>\n{changes}\n</changes>\n\n\
         Build/test/lint result:\n<verify>\n{verify}\n</verify>\n\n\
         Worker's report:\n<worker_report>\n{worker_report}\n</worker_report>\n{supervisor}{ledger}{history}\n\
         Output EXACTLY one JSON code block — decision is accept | rework | fail \
         (rework requires concrete, actionable feedback):\n```json\n{{\"decision\":\"accept\",\"feedback\":\"\"}}\n```"
    )
}

pub fn conduct_rework(
    original_prompt: &str,
    previous_changes: &str,
    feedback: &str,
    attempt_history: Option<&str>,
) -> String {
    let history = memory_block("attempt_history", attempt_history);
    format!(
        "A previous attempt at this subtask was rejected. Redo it correctly.\n\n\
         Original subtask:\n{original_prompt}\n\n\
         Previous attempt's changes:\n<changes>\n{previous_changes}\n</changes>\n\n\
         Reviewer feedback to address:\n{feedback}\n{history}\n\
         Apply the fixes on top of the current state of the repository."
    )
}

pub fn supervisor_gate(
    task: &str,
    progress: &str,
    plan_ledger: Option<&str>,
    attempt_history: Option<&str>,
) -> String {
    let ledger = memory_block("plan_ledger", plan_ledger);
    let history = memory_block("attempt_history", attempt_history);
    format!(
        "You are the supervisor of a multi-agent coding run. You read a lot and \
         intervene rarely — flag only real problems: scope drift, repeated \
         failures, destructive changes, work that contradicts the task.\n\n\
         Overall task:\n{task}\n\nProgress so far:\n<progress>\n{progress}\n</progress>\n{ledger}{history}\n\
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

pub fn arbiter_decide(
    subtask: &str,
    changes: &str,
    findings: &str,
    plan_ledger: Option<&str>,
    attempt_history: Option<&str>,
) -> String {
    let ledger = memory_block("plan_ledger", plan_ledger);
    let history = memory_block("attempt_history", attempt_history);
    format!(
        "You are the arbiter. A worker's subtask passed the conductor but the \
         reviewer keeps flagging critical findings after the rework limit. \
         Decide: ship (findings are tolerable or wrong) or fail (findings are \
         real blockers).\n\nSubtask:\n{subtask}\n\nFinal changes:\n<changes>\n{changes}\n</changes>\n\n\
         Reviewer findings:\n{findings}\n{ledger}{history}\n\
         Output EXACTLY one JSON code block — decision is ship | fail:\n```json\n{{\"decision\":\"ship\",\"reason\":\"\"}}\n```"
    )
}

pub fn diff_review(diff: &str) -> String {
    format!(
        "Review this diff for real problems. Actively HUNT for failure modes — do \
         not merely confirm it looks right: enumerate the edge cases and inputs the \
         change must handle (empty / zero / negative / overflow / unicode / \
         concurrent / error-and-timeout paths) and check each against the diff, then \
         flag any that are unhandled or untested — plus bugs, security issues, and \
         misleading naming. Do not invent style nitpicks. Then output EXACTLY one \
         JSON code block:\n```json\n{{\"findings\":[{{\"severity\":\"critical|important|minor\",\"file\":\"path\",\"description\":\"...\"}}]}}\n```\n\
         An empty findings array means you verified the diff is correct AND its edge \
         cases are handled — not merely that nothing obvious jumped out.\n\n\
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

    // TRINITY/Conductor borrows: the conductor prompt must demand concrete,
    // restated constraints and carry a few-shot exemplar (the largest ablation
    // delta in the Conductor paper); the reviewer must actively hunt edge cases.
    #[test]
    fn decompose_demands_concrete_constraints_with_exemplar() {
        let p = conduct_decompose("build a thing", "ctx");
        assert!(p.contains("RESTATE every concrete constraint"));
        assert!(p.contains("with_backoff"), "few-shot exemplar present");
        assert!(p.contains("build a thing") && p.contains("ctx"));
    }

    #[test]
    fn diff_review_demands_edge_case_hunt() {
        let p = diff_review("--- a/x");
        assert!(p.contains("HUNT"));
        assert!(p.to_lowercase().contains("edge case"));
        assert!(p.contains(r#""findings""#));
    }

    #[test]
    fn evaluation_omits_memory_blocks_when_none() {
        let p = conduct_evaluation("st", "ch", "wr", "v", None, None, None);
        assert!(!p.contains("<plan_ledger>"));
        assert!(!p.contains("<attempt_history>"));
    }

    #[test]
    fn evaluation_includes_memory_blocks_when_some() {
        let p = conduct_evaluation(
            "st",
            "ch",
            "wr",
            "v",
            None,
            Some("subtask 1 done"),
            Some("attempt 0: rework"),
        );
        assert!(p.contains("<plan_ledger>\nsubtask 1 done\n</plan_ledger>"));
        assert!(p.contains("<attempt_history>\nattempt 0: rework\n</attempt_history>"));
    }

    #[test]
    fn rework_includes_history_only_when_some() {
        let none = conduct_rework("op", "pc", "fb", None);
        assert!(!none.contains("<attempt_history>"));
        let some = conduct_rework("op", "pc", "fb", Some("attempt 0: rework"));
        assert!(some.contains("<attempt_history>\nattempt 0: rework\n</attempt_history>"));
        // rework is a worker prompt — it must never carry the cross-subtask ledger.
        assert!(!some.contains("<plan_ledger>"));
    }

    #[test]
    fn supervisor_and_arbiter_carry_ledger_when_some() {
        let sup = supervisor_gate("t", "p", Some("L"), None);
        assert!(sup.contains("<plan_ledger>\nL\n</plan_ledger>"));
        let arb = arbiter_decide("s", "c", "f", Some("L"), Some("H"));
        assert!(arb.contains("<plan_ledger>\nL\n</plan_ledger>"));
        assert!(arb.contains("<attempt_history>\nH\n</attempt_history>"));
    }

    #[test]
    fn initial_prompt_prepends_scope_discipline_without_blackboard() {
        let p = "Create src/foo.rs with a pub fn bar().";
        let out = conduct_initial(p, None);
        assert!(out.contains("Scope discipline"));
        assert!(out.contains(p));
        assert!(!out.contains("<prior_work>"), "no blackboard when None");
    }

    #[test]
    fn initial_prompt_wraps_blackboard_when_some() {
        let p = conduct_initial("do the thing", Some("- subtask 1 \"mathops\": completed"));
        assert!(p.contains("Scope discipline"));
        assert!(p.contains("do the thing"));
        assert!(p.contains("<prior_work>\n- subtask 1 \"mathops\": completed\n</prior_work>"));
        assert!(p.contains("read-only context"));
    }
}
