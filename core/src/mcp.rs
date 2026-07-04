//! MCP server — attached-conductor mode (M3a).
//!
//! In attached mode the *conductor is the user's live, interactive Claude Code
//! session*; this stdio MCP server exposes Consilium's worker + quota primitives
//! as tools so that session orchestrates the army (Codex/Gemini/fallback Claude)
//! WITHOUT spending programmatic Claude credit. The decision loop lives in the
//! subscription session; the engine just executes.
//!
//! Tools: `run_worker` and `quota_status` (M3a), plus `review_diff` (M3c Slice B)
//! and `council_run` — thin wrappers over existing library functions. Security
//! invariants: `run_worker` always builds `advisory:false, write:true` (it never
//! exposes an `advisory` knob), so the deliberation-grade trust relaxation can
//! never combine with auto-approved writes at the tool boundary (mirrors
//! sessions.rs); the `review_diff` and `council_run` paths are always
//! `advisory:true, write:false`; and every cwd-taking tool confines the
//! caller's `cwd` to the directory the server was launched in
//! ([`crate::confine::cwd_within_root`], mirroring the WS server) — the
//! conductor is an LLM steered by untrusted repo content, so an unconfined cwd
//! would let a prompt injection point write-enabled workers (and the auto-run
//! verifier's build/test commands) at an arbitrary directory.

use crate::adapters::RunRequest;
use crate::config::{Config, VerifyConfig};
use crate::event::Provider;
use crate::orchestrator::changes::capture_changes;
use crate::orchestrator::council::{run_council, CouncilMember};
use crate::orchestrator::resilience::{run_with_failover, ModelHealth, RetryConfig, Rung};
use crate::orchestrator::review::{run_review_ladder, Severity};
use crate::orchestrator::{roles, verify};
use crate::quota::{unix_now, QuotaStore};
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Sliding window for quota reporting, from the canonical
/// [`crate::quota::WINDOW_SECS`] (shared with the server's `/api/quota`).
const QUOTA_WINDOW_SECS: i64 = crate::quota::WINDOW_SECS;
const DEFAULT_WORKER_TIMEOUT_SECS: u64 = 600;
const DEFAULT_REVIEW_TIMEOUT_SECS: u64 = 900;
const DEFAULT_COUNCIL_TIMEOUT_SECS: u64 = 900;
/// Max chars of a `page_in` digest — keeps a huge transcript from flooding the
/// conductor's context.
const PAGE_IN_DIGEST_CAP: usize = 4000;

#[derive(Clone)]
pub struct McpServer {
    /// Workers resolved once at construction (label → failover ladder), mirroring
    /// `ConductDeps`. Pre-resolving makes the server injectable in tests.
    workers: Arc<Vec<CouncilMember>>,
    /// Optional build/test/lint verifier run after each worker (P0 #1 grounding).
    verify: Option<VerifyConfig>,
    /// Reviewer failover ladder for `review_diff` (the configured reviewer role).
    reviewer: Arc<Vec<Rung>>,
    /// Chairman failover ladder for `council_run` (the configured chairman role).
    chairman: Arc<Vec<Rung>>,
    transcript_base: PathBuf,
    health: ModelHealth,
    /// Shared quota store (internally `Sync`); reads/writes serialize on its
    /// own mutex, so concurrent tool calls are safe.
    quota: Arc<QuotaStore>,
    /// The directory the MCP server was launched in. Caller-supplied `cwd`
    /// values are validated to be within this root before any run is started
    /// (mirrors the WS server's confinement).
    launch_root: Arc<PathBuf>,
    tool_router: ToolRouter<Self>,
}

/// Named dependencies for constructing an [`McpServer`].
pub struct McpServerDeps {
    pub workers: Vec<CouncilMember>,
    pub reviewer: Vec<Rung>,
    pub chairman: Vec<Rung>,
    pub transcript_base: PathBuf,
    pub verify: Option<VerifyConfig>,
    pub quota: QuotaStore,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunWorkerParams {
    /// The full, self-contained instruction for the worker.
    pub prompt: String,
    /// Which configured worker to route to, as "provider-model"
    /// (e.g. "codex-gpt-5.5"); see the workers in consilium.config.json.
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

#[derive(Debug, Serialize, JsonSchema)]
pub struct RunWorkerOutput {
    /// True when a worker rung produced a result (NOT a verify verdict).
    pub ok: bool,
    /// "provider/model" that produced the result, if any.
    pub model_used: Option<String>,
    /// The worker's final report text.
    pub worker_report: Option<String>,
    /// Captured diff + new files after the worker ran.
    pub changes: Option<String>,
    /// Build/test/lint result when a verifier was configured/detected.
    pub verify: Option<VerifyReport>,
    /// Set when all worker rungs failed (the run never panics).
    pub error: Option<String>,
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
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FindingOut {
    /// "critical" | "important" | "minor".
    pub severity: String,
    pub file: String,
    pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
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

#[derive(Debug, Serialize, JsonSchema)]
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
}

#[tool_router(router = tool_router)]
impl McpServer {
    /// Build from config: resolve each configured worker into a label + failover
    /// ladder (real CLI adapters), matching how `consilium conduct` wires workers.
    pub fn new(config: Config, quota: QuotaStore) -> Self {
        let workers: Vec<CouncilMember> = config
            .roles
            .workers
            .iter()
            .map(|role| CouncilMember {
                label: format!("{}-{}", role.provider.as_str(), role.model),
                ladder: roles::resolve_ladder(role),
            })
            .collect();
        let reviewer = roles::resolve_ladder(&config.roles.reviewer);
        let chairman = roles::resolve_ladder(&config.roles.chairman);
        let transcript_base = crate::orchestrator::transcript::TranscriptStore::default_base()
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
        Self::from_parts(McpServerDeps {
            workers,
            reviewer,
            chairman,
            transcript_base,
            verify: config.verify,
            quota,
        })
    }

    /// Construct from already-resolved workers + reviewer ladder (used by tests to
    /// inject scripted adapters, and by `new` after resolving from config). The
    /// `launch_root` defaults to the process cwd; pass a custom one via
    /// [`McpServer::with_launch_root`] when a test uses a temp dir as cwd.
    pub fn from_parts(deps: McpServerDeps) -> Self {
        let McpServerDeps {
            workers,
            reviewer,
            chairman,
            transcript_base,
            verify,
            quota,
        } = deps;
        Self {
            workers: Arc::new(workers),
            reviewer: Arc::new(reviewer),
            chairman: Arc::new(chairman),
            transcript_base,
            verify,
            health: ModelHealth::with_retry(RetryConfig::prod()),
            quota: Arc::new(quota),
            launch_root: Arc::new(std::env::current_dir().unwrap_or_default()),
            tool_router: Self::tool_router(),
        }
    }

    /// Override the launch root (used in tests that run agents in a temp dir).
    pub fn with_launch_root(mut self, root: PathBuf) -> Self {
        self.launch_root = Arc::new(root);
        self
    }

    #[tool(
        name = "run_worker",
        description = "Route a self-contained subtask to a configured worker (Codex/Gemini/Claude), \
                       which edits files in `cwd`, then return the captured diff and build/test \
                       result. The worker runs with auto-approved writes; you (the conductor) decide \
                       what to delegate and whether to accept."
    )]
    pub async fn run_worker(
        &self,
        Parameters(p): Parameters<RunWorkerParams>,
    ) -> Json<RunWorkerOutput> {
        Json(self.run_worker_inner(p).await)
    }

    #[tool(
        name = "quota_status",
        description = "Report tokens used per provider (Claude / Codex / Gemini) in the last 5 hours, \
                       so you can route work to the freest subscription."
    )]
    pub async fn quota_status(&self) -> Json<QuotaStatusOutput> {
        Json(self.quota_status_inner())
    }

    #[tool(
        name = "review_diff",
        description = "Send a unified diff to the council's configured reviewer for a read-only \
                       audit, returning structured findings with severities. `ok` only means a \
                       reviewer ran — it is NOT a pass; a clean pass is `ok && parse_ok && \
                       !has_critical`. Treat `parse_ok:false` as an unusable review (fail closed) \
                       even when `ok:true`, and `has_critical:true` as a blocking verdict. For a \
                       true cross-family check, configure a reviewer of a different model family \
                       than the worker that wrote the diff; `model_used` reports who reviewed."
    )]
    pub async fn review_diff(
        &self,
        Parameters(p): Parameters<ReviewDiffParams>,
    ) -> Json<ReviewDiffOutput> {
        Json(self.review_diff_inner(p).await)
    }

    #[tool(
        name = "council_run",
        description = "Run the configured worker council on a question, then return the \
                       chairman's synthesis plus the member answers that informed it. This is \
                       read-only deliberation: the council runs with advisory:true and \
                       write:false."
    )]
    pub async fn council_run(
        &self,
        Parameters(p): Parameters<CouncilRunParams>,
    ) -> Json<CouncilRunOutput> {
        Json(self.council_run_inner(p).await)
    }

    #[tool(
        name = "search_recall",
        description = "Search across past run transcripts for a given query to recall \
                       previous tasks, summaries, and actions. This is read-only."
    )]
    pub async fn search_recall(
        &self,
        Parameters(p): Parameters<SearchRecallParams>,
    ) -> Json<SearchRecallOutput> {
        Json(self.search_recall_inner(p))
    }

    #[tool(
        name = "page_in",
        description = "Load a single past run transcript by id (e.g. an id from search_recall) and \
                       return a compact digest — task, outcome, and per-subtask titles/summaries — \
                       so you can recall that run's context. Read-only."
    )]
    pub async fn page_in(&self, Parameters(p): Parameters<PageInParams>) -> Json<PageInOutput> {
        Json(self.page_in_inner(p))
    }
}

impl McpServer {
    /// Search past transcripts for a query. Public for tests (the `search_recall`
    /// tool is a thin wrapper over this).
    pub fn search_recall_inner(&self, p: SearchRecallParams) -> SearchRecallOutput {
        if p.query.trim().is_empty() {
            return SearchRecallOutput {
                ok: false,
                hits: Vec::new(),
                error: Some("empty query: nothing to search for".into()),
            };
        }

        let store =
            crate::orchestrator::transcript::TranscriptStore::new(self.transcript_base.clone());
        let limit = p.limit.unwrap_or(10);
        let hits = store.search(&p.query, limit);

        SearchRecallOutput {
            ok: true,
            hits,
            error: None,
        }
    }

    /// Load a past transcript by id and digest it. Public for tests (the
    /// `page_in` tool is a thin wrapper over this).
    pub fn page_in_inner(&self, p: PageInParams) -> PageInOutput {
        let id = p.id.trim();
        if id.is_empty() {
            return PageInOutput {
                ok: false,
                id: None,
                kind: None,
                digest: None,
                error: Some("empty id: nothing to load".into()),
            };
        }
        let store =
            crate::orchestrator::transcript::TranscriptStore::new(self.transcript_base.clone());
        let Some(val) = store.load_by_id(id) else {
            return PageInOutput {
                ok: false,
                id: Some(id.to_string()),
                kind: None,
                digest: None,
                error: Some(format!("no transcript found for id '{id}'")),
            };
        };
        let kind = val.get("kind").and_then(|v| v.as_str()).map(str::to_string);
        PageInOutput {
            ok: true,
            id: Some(id.to_string()),
            kind,
            digest: Some(digest_transcript(&val)),
            error: None,
        }
    }

    /// Quota totals per provider over the reporting window. Public for tests.
    pub fn quota_status_inner(&self) -> QuotaStatusOutput {
        let since = unix_now() - QUOTA_WINDOW_SECS;
        let q = self.quota.as_ref();
        let totals = |provider: Provider| -> ProviderQuota {
            let (input_tokens, output_tokens) = q.totals_since(provider, since).unwrap_or((0, 0));
            let (est_in, est_out) = q.estimated_totals_since(provider, since).unwrap_or((0, 0));
            ProviderQuota {
                input_tokens,
                output_tokens,
                estimated: est_in + est_out > 0,
            }
        };
        QuotaStatusOutput {
            window_secs: QUOTA_WINDOW_SECS,
            claude: totals(Provider::Claude),
            codex: totals(Provider::Codex),
            gemini: totals(Provider::Gemini),
        }
    }

    /// Format the refusal for a `cwd` that failed confinement. One message for
    /// every cwd-taking tool, so the conductor gets consistent feedback.
    fn cwd_refusal(&self, requested: &std::path::Path) -> String {
        format!(
            "cwd '{}' is outside the directory the MCP server was launched in ('{}'); refusing to run",
            requested.display(),
            self.launch_root.display(),
        )
    }

    /// Route a subtask to the named worker, run it, capture changes + verify.
    /// Public for tests (the `run_worker` tool is a thin wrapper over this).
    pub async fn run_worker_inner(&self, p: RunWorkerParams) -> RunWorkerOutput {
        // SECURITY: the caller is an LLM conductor steered by whatever it reads
        // (repo content included), so `cwd` is untrusted. Confine it to the
        // launch root BEFORE anything runs — the worker writes files here and
        // `run_verify` executes build/test commands here.
        let cwd = PathBuf::from(&p.cwd);
        if !crate::confine::cwd_within_root(&cwd, &self.launch_root) {
            return RunWorkerOutput {
                ok: false,
                model_used: None,
                worker_report: None,
                changes: None,
                verify: None,
                error: Some(self.cwd_refusal(&cwd)),
            };
        }

        // Resolve the named worker (label = "provider-model").
        let Some(worker) = self.workers.iter().find(|w| w.label == p.worker_label) else {
            let known: Vec<String> = self.workers.iter().map(|w| w.label.clone()).collect();
            return RunWorkerOutput {
                ok: false,
                model_used: None,
                worker_report: None,
                changes: None,
                verify: None,
                error: Some(format!(
                    "unknown worker_label '{}'; configured workers: {}",
                    p.worker_label,
                    known.join(", ")
                )),
            };
        };

        let ladder = &worker.ladder;
        let timeout = Duration::from_secs(p.timeout_secs.unwrap_or(DEFAULT_WORKER_TIMEOUT_SECS));
        let prompt = p.prompt.clone();
        let cwd_for_req = cwd.clone();

        // SECURITY: write:true, advisory:false — never exposed as a parameter.
        let fo = run_with_failover(
            ladder,
            &p.worker_label,
            move |model| RunRequest {
                prompt: prompt.clone(),
                model,
                cwd: cwd_for_req.clone(),
                advisory: false,
                write: true,
            },
            self.quota.as_ref(),
            &self.health,
            timeout,
        )
        .await;

        let fo = match fo {
            Ok(fo) => fo,
            Err(e) => {
                return RunWorkerOutput {
                    ok: false,
                    model_used: None,
                    worker_report: None,
                    changes: None,
                    verify: None,
                    error: Some(e.to_string()),
                }
            }
        };

        // Capture changes + run the configured verifier, if any. NOTE: unlike the
        // library `capture_changes` (whose git error is load-bearing — a worker
        // that did nothing must not be accepted), here a git failure degrades to
        // `None` on purpose: the attached conductor reviews diffs itself and may
        // legitimately point a worker at a non-git cwd. Best-effort context.
        let changes = capture_changes(&cwd).ok();
        let verify_outcome = verify::run_verify(&cwd, self.verify.as_ref()).await;
        let verify = verify_outcome.ran.then_some(VerifyReport {
            ran: verify_outcome.ran,
            passed: verify_outcome.passed,
            summary: verify_outcome.summary,
        });

        RunWorkerOutput {
            ok: true,
            model_used: Some(fo.model_used),
            worker_report: Some(fo.outcome.final_text),
            changes,
            verify,
            error: None,
        }
    }

    /// Audit a diff with the reviewer ladder (read-only). Public for tests (the
    /// `review_diff` tool is a thin wrapper over this).
    pub async fn review_diff_inner(&self, p: ReviewDiffParams) -> ReviewDiffOutput {
        if p.diff.trim().is_empty() {
            return ReviewDiffOutput {
                ok: false,
                model_used: None,
                parse_ok: false,
                has_critical: false,
                findings: Vec::new(),
                raw_review: None,
                error: Some("empty diff: nothing to review".into()),
            };
        }
        // SECURITY: `cwd` is untrusted conductor input — confine to the launch
        // root before the reviewer process runs there (same check as run_worker).
        let cwd = p
            .cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| self.launch_root.as_ref().clone());
        if !crate::confine::cwd_within_root(&cwd, &self.launch_root) {
            return ReviewDiffOutput {
                ok: false,
                model_used: None,
                parse_ok: false,
                has_critical: false,
                findings: Vec::new(),
                raw_review: None,
                error: Some(self.cwd_refusal(&cwd)),
            };
        }
        let timeout = Duration::from_secs(p.timeout_secs.unwrap_or(DEFAULT_REVIEW_TIMEOUT_SECS));
        let ladder: &[Rung] = &self.reviewer;

        // advisory:true, write:false is baked into run_review_ladder — read-only.
        match run_review_ladder(
            &p.diff,
            ladder,
            &self.health,
            self.quota.as_ref(),
            cwd,
            timeout,
        )
        .await
        {
            Ok((result, _fallbacks)) => {
                let parse_ok = result.verdict.is_some();
                let has_critical = result.verdict.as_ref().is_some_and(|v| v.has_critical());
                let findings = result
                    .verdict
                    .map(|v| {
                        v.findings
                            .into_iter()
                            .map(|f| FindingOut {
                                severity: severity_str(&f.severity).into(),
                                file: f.file,
                                description: f.description,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                ReviewDiffOutput {
                    ok: true,
                    model_used: result.model_used,
                    parse_ok,
                    has_critical,
                    findings,
                    raw_review: Some(result.raw_review),
                    error: None,
                }
            }
            Err(e) => ReviewDiffOutput {
                ok: false,
                model_used: None,
                parse_ok: false,
                has_critical: false,
                findings: Vec::new(),
                raw_review: None,
                error: Some(e.to_string()),
            },
        }
    }

    /// Run the configured worker council with the configured chairman. Public for
    /// tests (the `council_run` tool is a thin wrapper over this).
    pub async fn council_run_inner(&self, p: CouncilRunParams) -> CouncilRunOutput {
        if p.question.trim().is_empty() {
            return CouncilRunOutput {
                ok: false,
                synthesis: None,
                answers: Vec::new(),
                failed_members: Vec::new(),
                error: Some("empty question: nothing to ask the council".into()),
            };
        }
        // SECURITY: `cwd` is untrusted conductor input — confine to the launch
        // root before council members run there (same check as run_worker).
        let cwd = p
            .cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| self.launch_root.as_ref().clone());
        if !crate::confine::cwd_within_root(&cwd, &self.launch_root) {
            return CouncilRunOutput {
                ok: false,
                synthesis: None,
                answers: Vec::new(),
                failed_members: Vec::new(),
                error: Some(self.cwd_refusal(&cwd)),
            };
        }
        let timeout = Duration::from_secs(p.timeout_secs.unwrap_or(DEFAULT_COUNCIL_TIMEOUT_SECS));
        let members: Vec<CouncilMember> = self
            .workers
            .iter()
            .map(|worker| CouncilMember {
                label: worker.label.clone(),
                ladder: worker.ladder.clone(),
            })
            .collect();
        let chairman = self.chairman.as_ref().clone();
        let health = ModelHealth::with_retry(RetryConfig::prod());

        match run_council(
            &p.question,
            members,
            chairman,
            self.quota.as_ref(),
            cwd,
            timeout,
            &health,
        )
        .await
        {
            Ok(outcome) => CouncilRunOutput {
                ok: true,
                synthesis: Some(outcome.synthesis),
                answers: outcome
                    .answers
                    .into_iter()
                    .map(|(_, member, answer)| AnswerOut { member, answer })
                    .collect(),
                failed_members: outcome.failed_members,
                error: None,
            },
            Err(e) => CouncilRunOutput {
                ok: false,
                synthesis: None,
                answers: Vec::new(),
                failed_members: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    }
}

/// A compact, size-capped digest of a transcript JSON: task, outcome, summary,
/// and per-subtask titles + summaries. Robust to missing fields / varied kinds.
fn digest_transcript(val: &serde_json::Value) -> String {
    let str_field = |k: &str| val.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let mut s = String::new();
    let task = str_field("task");
    if !task.is_empty() {
        s.push_str(&format!("task: {task}\n"));
    }
    if let Some(c) = val.get("completed").and_then(|v| v.as_array()) {
        s.push_str(&format!("completed: {} subtask(s)\n", c.len()));
    }
    let halted = str_field("halted");
    if !halted.is_empty() {
        s.push_str(&format!("halted: {halted}\n"));
    }
    let failed = str_field("failed");
    if !failed.is_empty() {
        s.push_str(&format!("failed: {failed}\n"));
    }
    let summary = str_field("summary");
    if !summary.is_empty() {
        s.push_str(&format!("summary: {summary}\n"));
    }
    if let Some(subs) = val.get("subtasks").and_then(|v| v.as_array()) {
        for st in subs {
            let title = st.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let sm = st.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            if !title.is_empty() || !sm.is_empty() {
                s.push_str(&format!("- {title}: {sm}\n"));
            }
        }
    }
    if s.chars().count() > PAGE_IN_DIGEST_CAP {
        let capped: String = s.chars().take(PAGE_IN_DIGEST_CAP).collect();
        format!("{capped}…[truncated]")
    } else {
        s
    }
}

fn severity_str(s: &Severity) -> &'static str {
    match s {
        Severity::Critical => "critical",
        Severity::Important => "important",
        Severity::Minor => "minor",
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo is #[non_exhaustive] — build from Default, then set fields.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Consilium attached-conductor MCP server. You are the conductor: decompose the \
             task yourself, delegate self-contained subtasks to workers via `run_worker`, and \
             check `quota_status` to route to the freest provider. Workers edit real files and \
             return diffs + build/test results. Use `review_diff` for a read-only audit of a diff \
             by the configured reviewer, `council_run` for a read-only worker-council \
             synthesis by the configured chairman, `search_recall` to query past runs, and \
             `page_in` to load a past run by id."
                .to_string(),
        );
        info
    }
}

/// Serve the attached-conductor MCP server over stdio until the client
/// disconnects. Loads config + the shared quota store and blocks.
pub async fn serve_stdio(config: Config, quota: QuotaStore) -> anyhow::Result<()> {
    use rmcp::transport::stdio;
    use rmcp::ServiceExt;

    let server = McpServer::new(config, quota);
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
