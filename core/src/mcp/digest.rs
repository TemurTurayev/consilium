//! The digest `page_in` returns: a past run squeezed into what the conductor
//! needs to recall it, without the full raw transcript.

use serde_json::Value;

/// Max chars of a `page_in` digest — keeps a huge transcript from flooding the
/// conductor's context.
const PAGE_IN_DIGEST_CAP: usize = 4000;

/// How much of a review's diff the digest repeats: enough to tell which files
/// it was about.
const DIFF_DIGEST_CHARS: usize = 400;

/// The text fields a digest reports, in order, as (transcript key, label).
/// Each kind of run fills its own: conduct/auto a task and a summary, a
/// council a question and a synthesis, a worker run a task and its report, a
/// review who reviewed and what they said.
const TEXT_FIELDS: [(&str, &str); 10] = [
    ("task", "task"),
    ("question", "question"),
    ("worker", "worker"),
    ("model_used", "model"),
    ("chairman_model", "chairman"),
    ("halted", "halted"),
    ("failed", "failed"),
    ("summary", "summary"),
    ("synthesis", "synthesis"),
    ("raw_review", "review"),
];

/// A compact, size-capped digest of a transcript JSON: what the run was about,
/// who took part, how it ended, and per-subtask titles + summaries. Robust to
/// missing fields / varied kinds.
pub(super) fn digest_transcript(val: &Value) -> String {
    let text = |key: &str| val.get(key).and_then(Value::as_str).unwrap_or("");
    let list = |key: &str| val.get(key).and_then(Value::as_array);
    let line =
        |label: &str, value: &str| (!value.is_empty()).then(|| format!("{label}: {value}\n"));

    let fields = TEXT_FIELDS
        .iter()
        .filter_map(|(key, label)| line(label, text(key)));
    let diff = line(
        "diff",
        &crate::text::one_line(text("diff_preview"), DIFF_DIGEST_CHARS),
    );
    let members = list("answers").and_then(|answers| {
        let names: Vec<String> = answers.iter().filter_map(member_and_model).collect();
        line("members", &names.join(", "))
    });
    let failed_members = list("failed_members").and_then(|failed| {
        let names: Vec<&str> = failed.iter().filter_map(Value::as_str).collect();
        line("failed members", &names.join(", "))
    });
    let completed = list("completed").map(|done| format!("completed: {} subtask(s)\n", done.len()));
    let subtasks = list("subtasks")
        .into_iter()
        .flatten()
        .filter_map(|subtask| {
            let field = |key: &str| subtask.get(key).and_then(Value::as_str).unwrap_or("");
            let (title, summary) = (field("title"), field("summary"));
            (!title.is_empty() || !summary.is_empty()).then(|| format!("- {title}: {summary}\n"))
        });

    let digest: String = fields
        .chain(diff)
        .chain(members)
        .chain(failed_members)
        .chain(completed)
        .chain(subtasks)
        .collect();
    if digest.chars().count() > PAGE_IN_DIGEST_CAP {
        let capped: String = digest.chars().take(PAGE_IN_DIGEST_CAP).collect();
        format!("{capped}…[truncated]")
    } else {
        digest
    }
}

/// A council member as "label (provider/model)" — or just the label when the
/// transcript predates model reporting.
fn member_and_model(answer: &Value) -> Option<String> {
    let member = answer.get("member").and_then(Value::as_str)?;
    Some(match answer.get("model_used").and_then(Value::as_str) {
        Some(model) => format!("{member} ({model})"),
        None => member.to_string(),
    })
}
