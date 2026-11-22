//! Parameters and results of the MCP tools — the shapes the attached conductor
//! sees. Field docs here become the tools' JSON-schema descriptions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunWorkerParams {
    /// The full, self-contained instruction for the worker.
    pub prompt: String,
    /// Which configured worker to route to, as "provider-model"
    /// (e.g. "codex-gpt-6.1-sol"). The `lineup` tool lists the valid labels.
    pub worker_label: String,
    /// Absolute path to the repository/working directory the worker edits.
    /// Must be inside the directory the MCP server was launched in.
    pub cwd: String,
    /// Per-attempt timeout in seconds (default 600).
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct VerifyReport {
    pub ran: bool,
    pub passed: bool,
    pub summary: String,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct RunWorkerOutput {
    /// True when a worker rung produced a result (NOT a verify verdict).
    pub ok: bool,
    /// "provider/model" that produced the result, if any.
    pub model_used: Option<String>,
    /// The worker's final report text.
    pub worker_report: Option<String>,
    /// What this worker changed: a diff of the tree before and after its run
    /// (new files included). Edits that were already uncommitted when it
    /// started — yours, or an earlier worker's — are not part of it.
    pub changes: Option<String>,
    /// Build/test/lint result when a verifier was configured/detected.
    pub verify: Option<VerifyReport>,
    /// Set when all worker rungs failed (the run never panics).
    pub error: Option<String>,
    /// Id of the saved transcript of this run — pass it to `page_in`.
    pub transcript_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewDiffParams {
    /// The unified diff to review (e.g. `git diff` output).
    pub diff: String,
    /// Absolute path the reviewer process runs in (read-only). Defaults to the
    /// directory the MCP server was launched in and must be inside it.
    pub cwd: Option<String>,
    /// Per-attempt timeout in seconds (default 900).
    pub timeout_secs: Option<u64>,
    /// Who wrote the diff — a `run_worker` result's `model_used`
    /// ("provider/model"), or that worker's label. A reviewer of a different
    /// model family is tried first. Leave it out for a diff you (the
    /// conductor) wrote yourself.
    pub author_model: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FindingOut {
    /// "critical" | "important" | "minor".
    pub severity: String,
    pub file: String,
    pub description: String,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct ReviewDiffOutput {
    /// True when a reviewer rung produced a result (NOT a clean verdict — see the
    /// tool description: a clean pass is `ok && parse_ok && !has_critical`).
    pub ok: bool,
    /// "provider/model" that produced the review, if any (the cross-family signal).
    pub model_used: Option<String>,
    /// Whether the reviewer's output parsed into structured findings. Treat
    /// `false` as an unusable review — fail closed.
    pub parse_ok: bool,
    /// True iff a parsed verdict contains a Critical finding (a blocking verdict).
    pub has_critical: bool,
    /// Structured findings (empty when clean or unparsed).
    pub findings: Vec<FindingOut>,
    /// The reviewer's raw text (the fallback when `parse_ok` is false).
    pub raw_review: Option<String>,
    /// Set when all reviewer rungs failed (the call never panics).
    pub error: Option<String>,
    /// Id of the saved transcript of this review — pass it to `page_in`.
    pub transcript_id: Option<String>,
    /// Whether a reviewer of a different model family than the diff's author
    /// was available: "applied" (one was put first), "degraded_same_family"
    /// (none exists in the lineup — the review may share the author's blind
    /// spots) or "unknown_author" (`author_model` named no model or worker
    /// this server knows, so the diff was reviewed as if it were yours).
    /// `model_used` names who actually reviewed.
    pub cross_family: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CouncilRunParams {
    /// The question to send through the configured worker council.
    pub question: String,
    /// Absolute path the council runs in (read-only). Defaults to the
    /// directory the MCP server was launched in and must be inside it.
    pub cwd: Option<String>,
    /// Per-attempt timeout in seconds (default 900).
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AnswerOut {
    pub member: String,
    pub answer: String,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct CouncilRunOutput {
    /// True when the council produced a synthesis.
    pub ok: bool,
    /// The chairman's synthesis when the council succeeded.
    pub synthesis: Option<String>,
    /// Member answers surfaced with de-anonymized member labels.
    pub answers: Vec<AnswerOut>,
    /// Members that failed to produce an answer.
    pub failed_members: Vec<String>,
    /// Set when validation failed or the council run failed (the call never panics).
    pub error: Option<String>,
    /// Every model demotion during the run, as "from → to (why)". Non-empty
    /// means the council did not run on the lineup as configured.
    pub fallbacks: Vec<String>,
    /// Id of the saved transcript of this council — pass it to `page_in`.
    pub transcript_id: Option<String>,
}

/// One worker of the lineup.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkerLineup {
    /// What `run_worker` takes as `worker_label`.
    pub label: String,
    /// The worker's models as "provider/model", tried in this order.
    pub models: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LineupOutput {
    /// The workers `run_worker` can route to. They are also the members a
    /// `council_run` asks.
    pub workers: Vec<WorkerLineup>,
    /// The chairman's models ("provider/model", in order) — who synthesizes a council.
    pub chairman: Vec<String>,
    /// The reviewer's models ("provider/model", in order) — who answers `review_diff`.
    pub reviewer: Vec<String>,
    /// The configuration file(s) this lineup was read from; absent when the
    /// built-in defaults apply.
    pub config: Option<String>,
    /// The directory the server was launched in. Every `cwd` must be inside it.
    pub launch_root: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchRecallParams {
    /// The query string to search for across past transcript runs.
    pub query: String,
    /// Maximum number of recent transcripts to return (default 10).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchRecallOutput {
    /// True when the search completed successfully.
    pub ok: bool,
    /// The matching transcripts, if any.
    pub hits: Vec<crate::orchestrator::transcript::SearchHit>,
    /// Set when the search fails (e.g., empty query).
    pub error: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PageInParams {
    /// The run id to load (e.g. a `search_recall` hit's `id`).
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PageInOutput {
    /// True when a transcript was found and loaded.
    pub ok: bool,
    /// The resolved run id (echoed back).
    pub id: Option<String>,
    /// The run kind (conduct/council/review/…), if present.
    pub kind: Option<String>,
    /// A compact, size-capped digest of the run (task, outcome, per-subtask
    /// titles + summaries) — enough to recall context without the full raw JSON.
    pub digest: Option<String>,
    /// Set on an empty id or when no transcript matches.
    pub error: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProviderQuota {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// True when these tokens are heuristic estimates (the provider's CLI reports
    /// no usage, e.g. Gemini via agy) rather than CLI-measured.
    pub estimated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QuotaStatusOutput {
    pub window_secs: i64,
    pub claude: ProviderQuota,
    pub codex: ProviderQuota,
    pub gemini: ProviderQuota,
    pub grok: ProviderQuota,
}

/// A tool result that reports a failure and nothing else.
macro_rules! failed_constructor {
    ($($output:ident),+) => {$(
        impl $output {
            pub(super) fn failed(error: impl Into<String>) -> Self {
                Self {
                    error: Some(error.into()),
                    ..Self::default()
                }
            }
        }
    )+};
}

failed_constructor!(RunWorkerOutput, ReviewDiffOutput, CouncilRunOutput);
