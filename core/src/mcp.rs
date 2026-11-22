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

mod digest;
mod types;

pub use types::*;

use crate::adapters::RunRequest;
use crate::config::{Config, VerifyConfig};
use crate::event::Provider;
use crate::family::ModelFamily;
use crate::orchestrator::changes::{capture_changes, repository_root, TreeSnapshots};
use crate::orchestrator::council::{run_council, CouncilMember};
use crate::orchestrator::cross_family::cross_family_ladder;
use crate::orchestrator::resilience::{
    run_with_failover, Fallback, ModelHealth, RetryConfig, Rung,
};
use crate::orchestrator::review::{run_review_ladder, Severity};
use crate::orchestrator::transcript::TranscriptStore;
use crate::orchestrator::{roles, verify};
use crate::quota::{unix_now, QuotaStore};
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Sliding window for quota reporting, from the canonical
/// [`crate::quota::WINDOW_SECS`] (shared with the server's `/api/quota`).
const QUOTA_WINDOW_SECS: i64 = crate::quota::WINDOW_SECS;
/// The family of the attached conductor: this server is driven by a live
/// Claude Code session, so a diff with no named author is Claude's.
const CONDUCTOR_FAMILY: ModelFamily = ModelFamily::Anthropic;
const DEFAULT_WORKER_TIMEOUT_SECS: u64 = 600;
const DEFAULT_REVIEW_TIMEOUT_SECS: u64 = 900;
const DEFAULT_COUNCIL_TIMEOUT_SECS: u64 = 900;

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
    /// The configuration file(s) the lineup was read from, for `lineup`.
    config_source: Option<String>,
    /// One lock per repository: write workers in it run one at a time (see
    /// `write_turn`).
    write_turns: Arc<std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
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

#[tool_router(router = tool_router)]
impl McpServer {
    /// Build from config: resolve each configured worker into a label + failover
    /// ladder (real CLI adapters), matching how `consilium conduct` wires workers.
    pub fn new(config: Config, quota: QuotaStore) -> Self {
        let workers = roles::resolve_workers(&config.roles.workers);
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
            config_source: None,
            write_turns: Arc::default(),
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

    /// Record which configuration file(s) the lineup was read from, for `lineup`.
    pub fn with_config_source(mut self, source: Option<String>) -> Self {
        self.config_source = source;
        self
    }

    /// The configured council. Public for tests (the `lineup` tool is a thin
    /// wrapper over this).
    pub fn lineup_inner(&self) -> LineupOutput {
        LineupOutput {
            workers: self
                .workers
                .iter()
                .map(|worker| WorkerLineup {
                    label: worker.label.clone(),
                    models: ladder_models(&worker.ladder),
                })
                .collect(),
            chairman: ladder_models(&self.chairman),
            reviewer: ladder_models(&self.reviewer),
            config: self.config_source.clone(),
            launch_root: self.launch_root.display().to_string(),
        }
    }

    #[tool(
        name = "lineup",
        description = "List the configured council: the worker labels `run_worker` accepts \
                       (each with its models, tried in order), the chairman and the reviewer, \
                       the configuration file(s) it was read from, and the directory every \
                       `cwd` must be inside. The lineup is read once, when the server starts: \
                       a configuration edit shows up after the session is restarted."
    )]
    pub async fn lineup(&self) -> Json<LineupOutput> {
        Json(self.lineup_inner())
    }

    #[tool(
        name = "run_worker",
        description = "Route a self-contained subtask to a configured worker (Codex/Gemini/Claude/Grok), \
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
        description = "Report tokens used per provider (Claude / Codex / Gemini / Grok) in the last 5 hours, \
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
                       even when `ok:true`, and `has_critical:true` as a blocking verdict. Pass \
                       `author_model` (a `run_worker` result's `model_used`) for a worker's diff: \
                       a reviewer of a different model family than the author is tried first, \
                       `cross_family` says whether one existed, and `model_used` reports who \
                       reviewed. Without it the diff is taken to be your own."
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
            digest: Some(digest::digest_transcript(&val)),
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
            grok: totals(Provider::Grok),
        }
    }

    /// The lock a write worker holds while it works in `cwd`'s repository (or
    /// in `cwd` itself outside one). Workers there edit one tree: run
    /// together, each would report the other's half-made changes as its own
    /// and build and test a tree that is still moving.
    fn write_turn(&self, cwd: &std::path::Path) -> Arc<tokio::sync::Mutex<()>> {
        let tree = repository_root(cwd)
            .or_else(|| cwd.canonicalize().ok())
            .unwrap_or_else(|| cwd.to_path_buf());
        let mut turns = self
            .write_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        turns.entry(tree).or_default().clone()
    }

    /// The family of a diff's author as the conductor names it: a configured
    /// worker's label (its primary model's family), or what a run reports as
    /// `model_used`. `None` when it is neither.
    fn family_of_author(&self, author: &str) -> Option<ModelFamily> {
        self.workers
            .iter()
            .find(|worker| worker.label == author)
            .and_then(|worker| worker.ladder.first())
            .map(|rung| ModelFamily::of(rung.candidate.provider, &rung.candidate.model))
            .or_else(|| ModelFamily::of_author(author))
    }

    /// The configured worker labels, comma-separated, for messages.
    fn worker_labels(&self) -> String {
        if self.workers.is_empty() {
            return "none".to_string();
        }
        let labels: Vec<&str> = self.workers.iter().map(|w| w.label.as_str()).collect();
        labels.join(", ")
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

    /// Saves a run where `search_recall` and `page_in` look for it, returning
    /// its id. Recall is a convenience: a transcript that cannot be written
    /// must not fail the run it describes.
    fn save_transcript(&self, kind: &str, payload: &serde_json::Value) -> Option<String> {
        let store = TranscriptStore::new(self.transcript_base.clone());
        match store.save(kind, payload) {
            Ok(path) => path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(String::from),
            Err(e) => {
                tracing::warn!(error = %e, kind, "could not save the run's transcript");
                None
            }
        }
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
            return RunWorkerOutput::failed(self.cwd_refusal(&cwd));
        }

        // Resolve the named worker (label = "provider-model").
        let Some(worker) = self.workers.iter().find(|w| w.label == p.worker_label) else {
            return RunWorkerOutput::failed(format!(
                "unknown worker_label '{}'; configured workers: {} (`lineup` shows their models)",
                p.worker_label,
                self.worker_labels()
            ));
        };

        let timeout = Duration::from_secs(p.timeout_secs.unwrap_or(DEFAULT_WORKER_TIMEOUT_SECS));
        let prompt = p.prompt.clone();
        let cwd_for_req = cwd.clone();

        // One write worker at a time per repository, from the snapshot below
        // to the verifier's last command.
        let turn = self.write_turn(&cwd);
        let _turn = turn.lock().await;
        // The tree as the worker finds it — whatever is uncommitted in it now
        // is someone else's work, not this worker's.
        let tree_before = off_the_runtime({
            let cwd = cwd.clone();
            move || {
                let snapshots = TreeSnapshots::open(&cwd).ok()?;
                let before = snapshots.take().ok()?;
                Some((snapshots, before))
            }
        })
        .await;

        // SECURITY: write:true, advisory:false — never exposed as a parameter.
        let fo = run_with_failover(
            &worker.ladder,
            &p.worker_label,
            move |model| RunRequest {
                prompt: prompt.clone(),
                model,
                cwd: cwd_for_req.clone(),
                advisory: false,
                write: true,
            },
            self.quota.as_ref(),
            &call_health(),
            timeout,
        )
        .await;

        let fo = match fo {
            Ok(fo) => fo,
            Err(e) => {
                let error = e.to_string();
                let transcript_id = self.save_transcript(
                    "worker",
                    &serde_json::json!({
                        "kind": "worker",
                        "task": p.prompt,
                        "worker": p.worker_label,
                        "failed": error,
                    }),
                );
                return RunWorkerOutput {
                    transcript_id,
                    ..RunWorkerOutput::failed(error)
                };
            }
        };

        // Capture changes + run the configured verifier, if any. NOTE: unlike the
        // library `capture_changes` (whose git error is load-bearing — a worker
        // that did nothing must not be accepted), here a git failure degrades to
        // `None` on purpose: the attached conductor reviews diffs itself and may
        // legitimately point a worker at a non-git cwd. Best-effort context.
        let changes = off_the_runtime({
            let cwd = cwd.clone();
            move || worker_changes(&cwd, tree_before)
        })
        .await;
        let verify_outcome = verify::run_verify(&cwd, self.verify.as_ref()).await;
        let verify = verify_outcome.ran.then_some(VerifyReport {
            ran: verify_outcome.ran,
            passed: verify_outcome.passed,
            summary: verify_outcome.summary,
        });

        let transcript_id = self.save_transcript(
            "worker",
            &serde_json::json!({
                "kind": "worker",
                "task": p.prompt,
                "worker": p.worker_label,
                "model_used": fo.model_used,
                "summary": fo.outcome.final_text,
                "changes_chars": changes.as_ref().map(String::len),
                "verify": verify.as_ref().map(|v| serde_json::json!({
                    "passed": v.passed,
                    "summary": v.summary,
                })),
                "fallbacks": fo.fallbacks.iter().map(describe_fallback).collect::<Vec<_>>(),
            }),
        );

        RunWorkerOutput {
            ok: true,
            model_used: Some(fo.model_used),
            worker_report: Some(fo.outcome.final_text),
            changes,
            verify,
            error: None,
            transcript_id,
        }
    }

    /// Audit a diff with the reviewer ladder (read-only). Public for tests (the
    /// `review_diff` tool is a thin wrapper over this).
    pub async fn review_diff_inner(&self, p: ReviewDiffParams) -> ReviewDiffOutput {
        if p.diff.trim().is_empty() {
            return ReviewDiffOutput::failed("empty diff: nothing to review");
        }
        // SECURITY: `cwd` is untrusted conductor input — confine to the launch
        // root before the reviewer process runs there (same check as run_worker).
        let cwd = p
            .cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| self.launch_root.as_ref().clone());
        if !crate::confine::cwd_within_root(&cwd, &self.launch_root) {
            return ReviewDiffOutput::failed(self.cwd_refusal(&cwd));
        }
        let timeout = Duration::from_secs(p.timeout_secs.unwrap_or(DEFAULT_REVIEW_TIMEOUT_SECS));

        // A reviewer of a different family than the author goes first. With no
        // author named, the diff is the conductor's own — a Claude session. An
        // author that is named but cannot be placed is reviewed the same way,
        // and reported as unknown rather than as checked.
        let named_author = p
            .author_model
            .as_deref()
            .map(str::trim)
            .filter(|author| !author.is_empty());
        let author = named_author.and_then(|author| self.family_of_author(author));
        let (ladder, same_family_only) = cross_family_ladder(
            &self.reviewer,
            &self.workers,
            author.unwrap_or(CONDUCTOR_FAMILY),
        );
        let cross_family = match (named_author, author, same_family_only) {
            (Some(_), None, _) => "unknown_author",
            (_, _, true) => "degraded_same_family",
            _ => "applied",
        };

        // advisory:true, write:false is baked into run_review_ladder — read-only.
        let reviewed = run_review_ladder(
            &p.diff,
            &ladder,
            &call_health(),
            self.quota.as_ref(),
            cwd,
            timeout,
        )
        .await;
        let (result, _fallbacks) = match reviewed {
            Ok(reviewed) => reviewed,
            Err(e) => return ReviewDiffOutput::failed(e.to_string()),
        };

        let transcript_id = self.save_transcript("review", &result.transcript);
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
            transcript_id,
            cross_family: Some(cross_family.to_string()),
        }
    }

    /// Run the configured worker council with the configured chairman. Public for
    /// tests (the `council_run` tool is a thin wrapper over this).
    pub async fn council_run_inner(&self, p: CouncilRunParams) -> CouncilRunOutput {
        if p.question.trim().is_empty() {
            return CouncilRunOutput::failed("empty question: nothing to ask the council");
        }
        // SECURITY: `cwd` is untrusted conductor input — confine to the launch
        // root before council members run there (same check as run_worker).
        let cwd = p
            .cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| self.launch_root.as_ref().clone());
        if !crate::confine::cwd_within_root(&cwd, &self.launch_root) {
            return CouncilRunOutput::failed(self.cwd_refusal(&cwd));
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

        let outcome = run_council(
            &p.question,
            members,
            chairman,
            self.quota.as_ref(),
            cwd,
            timeout,
            &call_health(),
        )
        .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => return CouncilRunOutput::failed(e.to_string()),
        };

        let transcript_id = self.save_transcript("council", &outcome.transcript);
        CouncilRunOutput {
            ok: true,
            synthesis: Some(outcome.synthesis),
            answers: outcome
                .answers
                .into_iter()
                .map(|(_, member, answer)| AnswerOut { member, answer })
                .collect(),
            failed_members: outcome.failed_members,
            error: None,
            fallbacks: outcome.fallbacks.iter().map(describe_fallback).collect(),
            transcript_id,
        }
    }
}

/// What the worker itself changed: the difference between the tree as it
/// found it (`tree_before`: the snapshots and the one taken before it ran) and
/// the tree now. Falls back to everything uncommitted when the tree could not
/// be snapshotted, and to `None` outside a git repository.
fn worker_changes(
    cwd: &std::path::Path,
    tree_before: Option<(TreeSnapshots, String)>,
) -> Option<String> {
    let own = tree_before.and_then(|(snapshots, before)| {
        let after = snapshots.take().ok()?;
        snapshots.changes_between(&before, &after).ok()
    });
    own.or_else(|| capture_changes(cwd).ok())
}

/// Runs blocking git work on a thread of its own, so a slow repository does
/// not stall the other tool calls this server is answering. `None` if that
/// thread could not finish — the callers treat it as "nothing captured".
async fn off_the_runtime<T: Send + 'static>(
    work: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    tokio::task::spawn_blocking(work).await.ok().flatten()
}

/// The failure memory for one tool call. The server lives as long as the
/// session that started it — hours — while "this model is dead", "this rung is
/// throttled" and "this CLI is signed out" are true only until someone fixes
/// them. Remembering them across calls would keep a provider skipped long
/// after the user signed back in.
fn call_health() -> ModelHealth {
    ModelHealth::with_retry(RetryConfig::prod())
}

/// A ladder's models as "provider/model", in the order they are tried.
fn ladder_models(ladder: &[Rung]) -> Vec<String> {
    ladder
        .iter()
        .map(|rung| {
            format!(
                "{}/{}",
                rung.candidate.provider.as_str(),
                rung.candidate.model
            )
        })
        .collect()
}

/// One model demotion in words: "from → to (why)".
fn describe_fallback(fallback: &Fallback) -> String {
    format!("{} → {} ({})", fallback.from, fallback.to, fallback.reason)
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
        info.instructions = Some(format!(
            "Consilium attached-conductor MCP server. You are the conductor: decompose the \
             task yourself, delegate self-contained subtasks to workers via `run_worker`, and \
             check `quota_status` to route to the freest provider. Workers edit real files and \
             return diffs + build/test results. Use `review_diff` for a read-only audit of a diff \
             by the configured reviewer, `council_run` for a read-only worker-council \
             synthesis by the configured chairman, `search_recall` to query past runs, and \
             `page_in` to load a past run by id. Configured workers (`worker_label` values): {}. \
             `lineup` shows each worker's models, the chairman and the reviewer.",
            self.worker_labels()
        ));
        info
    }
}

/// Serve the attached-conductor MCP server over stdio until the client
/// disconnects. Loads config + the shared quota store and blocks.
pub async fn serve_stdio(
    config: Config,
    quota: QuotaStore,
    config_source: Option<String>,
) -> anyhow::Result<()> {
    use rmcp::transport::stdio;
    use rmcp::ServiceExt;

    let server = McpServer::new(config, quota).with_config_source(config_source);
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
