use crate::adapters::{Adapter, FailureKind, RunRequest};
use crate::config::ModelCandidate;
use crate::event::Provider;
use crate::orchestrator::runner::{run_to_completion, RunOutcome, RunStatus};
use crate::quota::QuotaStore;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One ladder rung: a model candidate paired with the adapter that runs it.
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

/// Runs a role's ladder with failover. `build_req` takes the rung's model
/// (Some) and returns the RunRequest for that attempt. Demotes on
/// ModelUnavailable (marks dead) and RateLimited; retries Transient once on the
/// same rung before demoting. Every demotion is recorded in `fallbacks` and
/// logged to stderr. Errors only when every rung is exhausted.
pub async fn run_with_failover(
    ladder: &[Rung],
    label: &str,
    build_req: impl Fn(Option<String>) -> RunRequest,
    quota: &QuotaStore,
    health: &ModelHealth,
    timeout: Duration,
) -> anyhow::Result<FailoverResult> {
    let mut fallbacks: Vec<Fallback> = Vec::new();
    let n = ladder.len();

    for (i, rung) in ladder.iter().enumerate() {
        let model = &rung.candidate.model;
        let provider = rung.candidate.provider;

        // Skip models already known dead this run.
        if health.is_dead(provider, model) {
            if let Some(next) = ladder.get(i + 1) {
                let fb = Fallback {
                    from: key(&rung.candidate),
                    to: key(&next.candidate),
                    reason: format!("{label}: {} is known-dead this run", key(&rung.candidate)),
                };
                eprintln!("↳ {label} fell back: {} → {} (known-dead)", fb.from, fb.to);
                fallbacks.push(fb);
            }
            continue;
        }

        let attempt = |adapter: Arc<dyn Adapter>| {
            let req = build_req(Some(model.clone()));
            run_to_completion(adapter, req, quota, timeout)
        };

        // Transient gets one retry on the same rung before demoting.
        let mut outcome = attempt(rung.adapter.clone()).await?;
        if let RunStatus::Failed(e) = &outcome.status {
            if rung.adapter.classify_failure(e) == FailureKind::Transient {
                outcome = attempt(rung.adapter.clone()).await?;
            }
        }

        // Success on this rung → done.
        if matches!(outcome.status, RunStatus::Completed) {
            return Ok(FailoverResult {
                model_used: key(&rung.candidate),
                outcome,
                rung_used: i,
                fallbacks,
            });
        }

        // Failure → classify, mark dead if permanent, record the demotion.
        let kind = match &outcome.status {
            RunStatus::Failed(e) => rung.adapter.classify_failure(e),
            RunStatus::TimedOut => FailureKind::Transient, // already retried above
            RunStatus::Completed => unreachable!("handled above"),
        };
        if kind == FailureKind::ModelUnavailable {
            health.mark_dead(provider, model);
        }
        let reason = match kind {
            FailureKind::ModelUnavailable => "model unavailable",
            FailureKind::RateLimited => "rate limited",
            FailureKind::Transient => "transient failure",
        };
        if let Some(next) = ladder.get(i + 1) {
            let fb = Fallback {
                from: key(&rung.candidate),
                to: key(&next.candidate),
                reason: format!("{label}: {} ({reason})", key(&rung.candidate)),
            };
            eprintln!("↳ {label} fell back: {} → {} ({reason})", fb.from, fb.to);
            fallbacks.push(fb);
        }
        // Last rung with no successor → loop ends, bail below.
    }

    anyhow::bail!("{label}: all {n} model rungs failed");
}
