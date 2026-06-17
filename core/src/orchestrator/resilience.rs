use crate::adapters::{Adapter, FailureKind, RunRequest};
use crate::config::ModelCandidate;
use crate::event::Provider;
use crate::orchestrator::runner::{run_to_completion, RunOutcome, RunStatus};
use crate::quota::QuotaStore;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One ladder rung: a model candidate paired with the adapter that runs it.
/// `Clone` is cheap — the adapter is an `Arc` (cross-family review reorders a
/// ladder into a new Vec without touching the adapters).
#[derive(Clone)]
pub struct Rung {
    pub candidate: ModelCandidate,
    pub adapter: Arc<dyn Adapter>,
}

/// Per-run registry of models proven dead (ModelUnavailable). Shared across all
/// roles in a run so a model pulled mid-run is skipped everywhere afterward.
#[derive(Clone, Default)]
pub struct ModelHealth {
    dead: Arc<Mutex<HashSet<(Provider, String)>>>,
}

impl ModelHealth {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn mark_dead(&self, provider: Provider, model: &str) {
        self.dead
            .lock()
            .unwrap()
            .insert((provider, model.to_string()));
    }
    pub fn is_dead(&self, provider: Provider, model: &str) -> bool {
        self.dead
            .lock()
            .unwrap()
            .contains(&(provider, model.to_string()))
    }
}

/// A single demotion, recorded for the transcript and surfaced on stderr.
#[derive(Debug, Clone)]
pub struct Fallback {
    pub from: String,   // "provider/model"
    pub to: String,     // "provider/model"
    pub reason: String, // human-readable
}

#[derive(Debug)]
pub struct FailoverResult {
    pub outcome: RunOutcome,
    pub rung_used: usize,
    pub fallbacks: Vec<Fallback>,
    /// The model that ultimately produced the outcome ("provider/model").
    pub model_used: String,
}

fn key(c: &ModelCandidate) -> String {
    format!("{}/{}", c.provider.as_str(), c.model)
}

/// Classifies a single rung attempt into a [`FailureKind`] (or `None` on
/// success). A spawn/launch `Err` (missing binary, PATH, EACCES) is treated as
/// `Transient` — it is local to this attempt, NOT evidence the model is dead, so
/// it must never mark the model dead. A `TimedOut` is likewise `Transient`.
fn classify_attempt(
    adapter: &Arc<dyn Adapter>,
    result: &anyhow::Result<RunOutcome>,
) -> Option<FailureKind> {
    match result {
        Ok(o) => match &o.status {
            RunStatus::Completed => None,
            RunStatus::Failed(e) => Some(adapter.classify_failure(e)),
            RunStatus::TimedOut => Some(FailureKind::Transient),
        },
        // Launch failure: local/transient, do NOT mark the model dead.
        Err(_) => Some(FailureKind::Transient),
    }
}

/// Runs a role's ladder with failover. `build_req` takes the rung's model
/// (Some) and returns the RunRequest for that attempt. Demotes on
/// ModelUnavailable (marks dead) and RateLimited; retries Transient once on the
/// same rung — which covers both a transient Failed event AND a spawn/launch
/// Err — before demoting. A rung attempt NEVER propagates with `?`: a spawn
/// error demotes to the next rung rather than aborting the whole ladder. Every
/// rung failure is logged loudly to stderr (demotions also recorded in
/// `fallbacks`); the bail carries every rung's failure reason. Errors only when
/// every rung is exhausted.
pub async fn run_with_failover(
    ladder: &[Rung],
    label: &str,
    build_req: impl Fn(Option<String>) -> RunRequest,
    quota: &QuotaStore,
    health: &ModelHealth,
    timeout: Duration,
) -> anyhow::Result<FailoverResult> {
    if ladder.is_empty() {
        anyhow::bail!("{label}: no model rungs configured");
    }
    let mut fallbacks: Vec<Fallback> = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let n = ladder.len();

    for (i, rung) in ladder.iter().enumerate() {
        let model = &rung.candidate.model;
        let provider = rung.candidate.provider;

        // Skip models already known dead this run.
        if health.is_dead(provider, model) {
            reasons.push(format!("{} known-dead", key(&rung.candidate)));
            if let Some(next) = ladder.get(i + 1) {
                let fb = Fallback {
                    from: key(&rung.candidate),
                    to: key(&next.candidate),
                    reason: format!("{label}: {} is known-dead this run", key(&rung.candidate)),
                };
                eprintln!("↳ {label} fell back: {} → {} (known-dead)", fb.from, fb.to);
                fallbacks.push(fb);
            } else {
                eprintln!("✗ {label}: {} failed (known-dead)", key(&rung.candidate));
            }
            continue;
        }

        let attempt = |adapter: Arc<dyn Adapter>| {
            let req = build_req(Some(model.clone()));
            run_to_completion(adapter, req, quota, timeout)
        };

        // First attempt — capture the Result WITHOUT `?` so a spawn/launch Err
        // demotes to the next rung instead of aborting the entire ladder.
        let mut result: anyhow::Result<RunOutcome> = attempt(rung.adapter.clone()).await;
        let mut kind = classify_attempt(&rung.adapter, &result);

        // Transient (transient Failed event OR spawn-err OR timeout) gets one
        // retry on the same rung before demoting.
        if kind == Some(FailureKind::Transient) {
            result = attempt(rung.adapter.clone()).await;
            kind = classify_attempt(&rung.adapter, &result);
        }

        // Success on this rung → done.
        let Some(kind) = kind else {
            return Ok(FailoverResult {
                model_used: key(&rung.candidate),
                outcome: result.expect("classify_attempt returned None only for Ok(Completed)"),
                rung_used: i,
                fallbacks,
            });
        };

        // Failure → mark dead only on ModelUnavailable (NOT rate-limited,
        // timed-out, or spawn-err), record a loud failure line + the demotion.
        if kind == FailureKind::ModelUnavailable {
            health.mark_dead(provider, model);
        }
        let reason = match kind {
            FailureKind::ModelUnavailable => "model unavailable",
            FailureKind::RateLimited => "rate limited",
            FailureKind::Transient => "transient failure",
        };
        reasons.push(format!("{} ({reason})", key(&rung.candidate)));
        if let Some(next) = ladder.get(i + 1) {
            let fb = Fallback {
                from: key(&rung.candidate),
                to: key(&next.candidate),
                reason: format!("{label}: {} ({reason})", key(&rung.candidate)),
            };
            eprintln!("↳ {label} fell back: {} → {} ({reason})", fb.from, fb.to);
            fallbacks.push(fb);
        } else {
            // Terminal rung: no successor to demote to, but the failure is still
            // logged loudly (the docstring promises every failure is loud).
            eprintln!("✗ {label}: {} failed ({reason})", key(&rung.candidate));
        }
    }

    anyhow::bail!(
        "{label}: all {n} model rungs failed: {}",
        reasons.join("; ")
    );
}
