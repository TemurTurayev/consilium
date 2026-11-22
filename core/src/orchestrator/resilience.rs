use crate::adapters::{Adapter, FailureKind, RunRequest};
use crate::config::ModelCandidate;
use crate::event::Provider;
use crate::orchestrator::runner::{run_to_completion, RunOutcome, RunStatus};
use crate::quota::QuotaStore;
use std::collections::{HashMap, HashSet};
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

/// Retry/backoff policy for a rung that fails Transient or RateLimited. Carried
/// on [`ModelHealth`] so it threads to [`run_with_failover`] with no call-site
/// change. The default is the historical behavior (one same-rung retry, no
/// sleep) so existing callers and tests are unaffected and fast; production
/// entry points opt into [`RetryConfig::prod`].
#[derive(Clone, Copy)]
pub struct RetryConfig {
    /// Base backoff before a same-rung retry; doubles each retry (capped at
    /// 60s). ZERO disables sleeping.
    pub backoff_base: Duration,
    /// Max same-rung retries on Transient/RateLimited before demoting.
    pub max_retries: u32,
    /// After this many RateLimited rung-failures ACROSS the run, the rung is
    /// marked "cold" and skipped fast for the rest of the run (circuit-breaker).
    /// `0` disables the breaker.
    pub cold_after: u32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            backoff_base: Duration::ZERO,
            max_retries: 1,
            cold_after: 0,
        }
    }
}

impl RetryConfig {
    /// Production policy: a brief throttle on the conductor (which is a single
    /// Claude-family ladder) or any rung should not kill the run — retry the
    /// same rung a few times with exponential backoff before demoting. (Designed
    /// fresh — the claw-code-agent review confirmed it has no backoff to copy.)
    pub fn prod() -> Self {
        Self {
            backoff_base: Duration::from_secs(2),
            max_retries: 3,
            cold_after: 3,
        }
    }

    /// Backoff before same-rung retry `idx` (0-based): `base * 2^idx`, capped at 60s.
    fn backoff_for(&self, idx: u32) -> Duration {
        if self.backoff_base.is_zero() {
            return Duration::ZERO;
        }
        let mult = 1u32.checked_shl(idx).unwrap_or(u32::MAX);
        self.backoff_base
            .saturating_mul(mult)
            .min(Duration::from_secs(60))
    }
}

/// Per-run registry of models proven dead (ModelUnavailable) plus the run's
/// retry/backoff policy. Shared across all roles in a run so a model pulled
/// mid-run is skipped everywhere afterward.
#[derive(Clone, Default)]
pub struct ModelHealth {
    dead: Arc<Mutex<HashSet<(Provider, String)>>>,
    /// Rungs tripped by the rate-limit circuit-breaker — skipped for the rest of
    /// the run once they hit `RetryConfig.cold_after` RateLimited failures. Unlike
    /// `dead`, the model isn't broken; we just stop paying to retry a throttled
    /// rung this run.
    cold: Arc<Mutex<HashSet<(Provider, String)>>>,
    rate_limit_hits: Arc<Mutex<HashMap<(Provider, String), u32>>>,
    /// Providers whose CLI turned out not to be signed in. Unlike `dead` this
    /// is about the provider, not one model: no rung of it can answer until
    /// the operator signs in, so all of them are skipped for the rest of the run.
    signed_out: Arc<Mutex<HashSet<Provider>>>,
    retry: RetryConfig,
}

impl ModelHealth {
    pub fn new() -> Self {
        Self::default()
    }
    /// Construct with an explicit retry/backoff policy (production entry points
    /// pass [`RetryConfig::prod`] to enable backoff under throttle).
    pub fn with_retry(retry: RetryConfig) -> Self {
        Self {
            dead: Arc::default(),
            cold: Arc::default(),
            rate_limit_hits: Arc::default(),
            signed_out: Arc::default(),
            retry,
        }
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
    /// Record that a provider's CLI is not signed in (see `signed_out`).
    pub fn mark_signed_out(&self, provider: Provider) {
        self.signed_out.lock().unwrap().insert(provider);
    }
    pub fn is_signed_out(&self, provider: Provider) -> bool {
        self.signed_out.lock().unwrap().contains(&provider)
    }
    /// True once a rung has been tripped cold by the rate-limit circuit-breaker.
    pub fn is_cold(&self, provider: Provider, model: &str) -> bool {
        self.cold
            .lock()
            .unwrap()
            .contains(&(provider, model.to_string()))
    }
    /// Record a RateLimited rung-failure; returns true if it just tripped the
    /// circuit-breaker (marked the rung cold). No-op when `cold_after == 0`.
    fn record_rate_limit(&self, provider: Provider, model: &str) -> bool {
        if self.retry.cold_after == 0 {
            return false;
        }
        let k = (provider, model.to_string());
        let mut hits = self.rate_limit_hits.lock().unwrap();
        let c = hits.entry(k.clone()).or_insert(0);
        *c += 1;
        if *c >= self.retry.cold_after {
            self.cold.lock().unwrap().insert(k);
            true
        } else {
            false
        }
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

/// How much of a CLI's own explanation a failure reason carries.
const FAILURE_DETAIL_CHARS: usize = 240;

/// Why a rung is not even attempted, if the run already knows it cannot answer.
fn skip_reason(health: &ModelHealth, candidate: &ModelCandidate) -> Option<String> {
    let ModelCandidate { provider, model } = candidate;
    if health.is_signed_out(*provider) {
        Some(format!("{} is not signed in", provider.as_str()))
    } else if health.is_dead(*provider, model) {
        Some("known-dead this run".to_string())
    } else if health.is_cold(*provider, model) {
        Some("rate-limit-cold this run".to_string())
    } else {
        None
    }
}

/// Runs one rung, retrying it in place while that can help. Transient (a
/// transient Failed event, a spawn error or a timeout) AND RateLimited get up
/// to `max_retries` same-rung retries with exponential backoff before demoting
/// — a brief throttle/blip shouldn't burn the rung. This matters most for the
/// conductor, whose ladder is single Claude-family: an instant demote+bail
/// under throttle killed whole runs. ModelUnavailable and Unauthenticated are
/// never retried: the same call cannot come out differently.
///
/// The attempt's Result is captured WITHOUT `?` so a spawn/launch Err demotes
/// to the next rung instead of aborting the entire ladder.
async fn attempt_with_retries(
    rung: &Rung,
    build_req: &impl Fn(Option<String>) -> RunRequest,
    quota: &QuotaStore,
    health: &ModelHealth,
    timeout: Duration,
) -> (anyhow::Result<RunOutcome>, Option<FailureKind>) {
    let attempt = || {
        let req = build_req(Some(rung.candidate.model.clone()));
        run_to_completion(rung.adapter.clone(), req, quota, timeout)
    };
    let mut result = attempt().await;
    let mut kind = classify_attempt(&rung.adapter, &result);
    let mut retries = 0u32;
    while matches!(
        kind,
        Some(FailureKind::Transient | FailureKind::RateLimited)
    ) && retries < health.retry.max_retries
    {
        // A genuine TimedOut (classified Transient) is unlikely to recover on
        // retry, and EACH retry costs a full `timeout` — cap it at one retry
        // regardless of max_retries so a hung rung demotes promptly. Rate
        // limits and transient network blips still get the full retry budget.
        if retries >= 1 && matches!(&result, Ok(o) if o.status == RunStatus::TimedOut) {
            break;
        }
        let backoff = health.retry.backoff_for(retries);
        if !backoff.is_zero() {
            tokio::time::sleep(backoff).await;
        }
        result = attempt().await;
        kind = classify_attempt(&rung.adapter, &result);
        retries += 1;
    }
    (result, kind)
}

/// Remembers what a rung's failure means for the rest of the run: a missing
/// model is dead, a signed-out CLI takes its whole provider with it, repeated
/// throttling trips the circuit-breaker. A rate-limited, timed-out or
/// spawn-failed rung is NOT marked dead.
fn remember_failure(
    health: &ModelHealth,
    label: &str,
    candidate: &ModelCandidate,
    kind: FailureKind,
) {
    let ModelCandidate { provider, model } = candidate;
    match kind {
        FailureKind::ModelUnavailable => health.mark_dead(*provider, model),
        FailureKind::Unauthenticated => health.mark_signed_out(*provider),
        FailureKind::RateLimited => {
            if health.record_rate_limit(*provider, model) {
                eprintln!(
                    "🧊 {label}: {} tripped the rate-limit circuit-breaker — cold for the rest of the run",
                    key(candidate)
                );
            }
        }
        FailureKind::Transient => {}
    }
}

/// A failed attempt in words: what kind of failure it was, then what the CLI
/// itself said, on one line. "transient failure" alone left the operator
/// nothing to act on.
fn describe_failure(
    kind: FailureKind,
    result: &anyhow::Result<RunOutcome>,
    timeout: Duration,
) -> String {
    let reason = match kind {
        FailureKind::ModelUnavailable => "model unavailable",
        FailureKind::RateLimited => "rate limited",
        FailureKind::Transient => "transient failure",
        FailureKind::Unauthenticated => "not signed in",
    };
    let said = match result {
        Ok(outcome) => match &outcome.status {
            RunStatus::Failed(error) => error.clone(),
            RunStatus::TimedOut => format!("timed out after {}s", timeout.as_secs()),
            RunStatus::Completed => String::new(),
        },
        Err(error) => error.to_string(),
    };
    let said = crate::text::one_line(&said, FAILURE_DETAIL_CHARS);
    if said.is_empty() {
        reason.to_string()
    } else {
        format!("{reason}: {said}")
    }
}

/// Runs a role's ladder with failover. `build_req` takes the rung's model
/// (Some) and returns the RunRequest for that attempt. A rung the run already
/// knows cannot answer is skipped (see `skip_reason`); the others are run with
/// same-rung retries where those can help (see `attempt_with_retries`), and
/// what each failure means is remembered for the rest of the run (see
/// `remember_failure`). A rung attempt NEVER propagates with `?`: a spawn
/// error demotes to the next rung rather than aborting the whole ladder. Every
/// rung failure is logged loudly to stderr (demotions also recorded in
/// `fallbacks`) with the CLI's own explanation; the bail carries every rung's
/// failure reason. Errors only when every rung is exhausted.
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

    for (i, rung) in ladder.iter().enumerate() {
        let candidate = &rung.candidate;
        let failure = match skip_reason(health, candidate) {
            Some(skipped) => skipped,
            None => {
                let (result, kind) =
                    attempt_with_retries(rung, &build_req, quota, health, timeout).await;
                let Some(kind) = kind else {
                    return Ok(FailoverResult {
                        model_used: key(candidate),
                        outcome: result
                            .expect("classify_attempt returned None only for Ok(Completed)"),
                        rung_used: i,
                        fallbacks,
                    });
                };
                remember_failure(health, label, candidate, kind);
                describe_failure(kind, &result, timeout)
            }
        };

        reasons.push(format!("{} ({failure})", key(candidate)));
        match ladder.get(i + 1) {
            Some(next) => {
                let fb = Fallback {
                    from: key(candidate),
                    to: key(&next.candidate),
                    reason: format!("{label}: {} ({failure})", key(candidate)),
                };
                eprintln!("↳ {label} fell back: {} → {} ({failure})", fb.from, fb.to);
                fallbacks.push(fb);
            }
            // Terminal rung: no successor to demote to, but the failure is still
            // logged loudly (the docstring promises every failure is loud).
            None => eprintln!("✗ {label}: {} failed ({failure})", key(candidate)),
        }
    }

    let sign_in_hint = if ladder
        .iter()
        .any(|rung| health.is_signed_out(rung.candidate.provider))
    {
        " — run `consilium auth` to see how to sign in"
    } else {
        ""
    };
    anyhow::bail!(
        "{label}: all {} model rungs failed: {}{sign_in_hint}",
        ladder.len(),
        reasons.join("; ")
    );
}
