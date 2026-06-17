//! MCP server — attached-conductor mode (M3a).
//!
//! In attached mode the *conductor is the user's live, interactive Claude Code
//! session*; this stdio MCP server exposes Consilium's worker + quota primitives
//! as tools so that session orchestrates the army (Codex/Gemini/fallback Claude)
//! WITHOUT spending programmatic Claude credit. The decision loop lives in the
//! subscription session; the engine just executes.
//!
//! M3a ships exactly two tools — `run_worker` and `quota_status` — both thin
//! wrappers over existing library functions. Security invariant: `run_worker`
//! always builds `advisory:false, write:true` (it never exposes an `advisory`
//! knob), so the deliberation-grade trust relaxation can never combine with
//! auto-approved writes at the tool boundary (mirrors sessions.rs).

use crate::adapters::RunRequest;
use crate::config::{Config, VerifyConfig};
use crate::event::Provider;
use crate::orchestrator::changes::capture_changes;
use crate::orchestrator::council::CouncilMember;
use crate::orchestrator::resilience::{run_with_failover, ModelHealth};
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

/// Sliding window for quota reporting (5 hours), matching the CLI `quota` view.
const QUOTA_WINDOW_SECS: i64 = 5 * 3600;
const DEFAULT_WORKER_TIMEOUT_SECS: u64 = 600;

#[derive(Clone)]
pub struct McpServer {
    /// Workers resolved once at construction (label → failover ladder), mirroring
    /// `ConductDeps`. Pre-resolving makes the server injectable in tests.
    workers: Arc<Vec<CouncilMember>>,
    /// Optional build/test/lint verifier run after each worker (P0 #1 grounding).
    verify: Option<VerifyConfig>,
    health: ModelHealth,
    /// Shared quota store (internally `Sync`); reads/writes serialize on its
    /// own mutex, so concurrent tool calls are safe.
    quota: Arc<QuotaStore>,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunWorkerParams {
    /// The full, self-contained instruction for the worker.
    pub prompt: String,
    /// Which configured worker to route to, as "provider-model"
    /// (e.g. "codex-gpt-5.4"); see the workers in consilium.config.json.
    pub worker_label: String,
    /// Absolute path to the repository/working directory the worker edits.
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

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProviderQuota {
    pub input_tokens: u64,
    pub output_tokens: u64,
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
        Self::from_parts(workers, config.verify, quota)
    }

    /// Construct from already-resolved workers (used by tests to inject scripted
    /// adapters, and by `new` after resolving from config).
    pub fn from_parts(
        workers: Vec<CouncilMember>,
        verify: Option<VerifyConfig>,
        quota: QuotaStore,
    ) -> Self {
        Self {
            workers: Arc::new(workers),
            verify,
            health: ModelHealth::new(),
            quota: Arc::new(quota),
            tool_router: Self::tool_router(),
        }
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
}

impl McpServer {
    /// Quota totals per provider over the reporting window. Public for tests.
    pub fn quota_status_inner(&self) -> QuotaStatusOutput {
        let since = unix_now() - QUOTA_WINDOW_SECS;
        let q = self.quota.as_ref();
        let totals = |provider: Provider| -> ProviderQuota {
            let (input_tokens, output_tokens) = q.totals_since(provider, since).unwrap_or((0, 0));
            ProviderQuota {
                input_tokens,
                output_tokens,
            }
        };
        QuotaStatusOutput {
            window_secs: QUOTA_WINDOW_SECS,
            claude: totals(Provider::Claude),
            codex: totals(Provider::Codex),
            gemini: totals(Provider::Gemini),
        }
    }

    /// Route a subtask to the named worker, run it, capture changes + verify.
    /// Public for tests (the `run_worker` tool is a thin wrapper over this).
    pub async fn run_worker_inner(&self, p: RunWorkerParams) -> RunWorkerOutput {
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
        let cwd = PathBuf::from(&p.cwd);
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

        // Capture changes (best-effort) + run the configured verifier, if any.
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
             return diffs + build/test results."
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
