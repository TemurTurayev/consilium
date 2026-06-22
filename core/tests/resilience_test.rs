mod common;

use common::{ScriptedAdapter, SequencedAdapter, SpawnFailAdapter};
use consilium::adapters::{Adapter, RunRequest};
use consilium::config::ModelCandidate;
use consilium::event::Provider;
use consilium::orchestrator::resilience::{run_with_failover, ModelHealth, RetryConfig, Rung};
use consilium::quota::QuotaStore;
use std::sync::Arc;
use std::time::Duration;

fn rung(provider: Provider, model: &str, adapter: Arc<dyn Adapter>) -> Rung {
    Rung {
        candidate: ModelCandidate {
            provider,
            model: model.into(),
        },
        adapter,
    }
}

fn req(model: Option<String>) -> RunRequest {
    RunRequest {
        prompt: "q".into(),
        model,
        cwd: std::env::temp_dir(),
        advisory: true,
        write: false,
    }
}

#[tokio::test]
async fn first_rung_success_no_fallback() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let ladder = vec![
        rung(
            Provider::Claude,
            "opus",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "done")),
        ),
        rung(
            Provider::Claude,
            "sonnet",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "unused")),
        ),
    ];
    let res = run_with_failover(
        &ladder,
        "lbl",
        req,
        &store,
        &health,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome.final_text, "done");
    assert!(res.fallbacks.is_empty());
    assert_eq!(res.rung_used, 0);
}

#[tokio::test]
async fn model_unavailable_demotes_loudly_and_marks_dead() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let ladder = vec![
        rung(
            Provider::Claude, "claude-fable-5",
            Arc::new(ScriptedAdapter::failing(Provider::Claude, "issue with the selected model (claude-fable-5). It may not exist or you may not have access to it.")),
        ),
        rung(Provider::Claude, "claude-opus-4-8", Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "recovered"))),
    ];
    let res = run_with_failover(
        &ladder,
        "conductor",
        req,
        &store,
        &health,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome.final_text, "recovered");
    assert_eq!(res.rung_used, 1);
    assert_eq!(res.fallbacks.len(), 1);
    assert!(res.fallbacks[0].reason.contains("unavailable"));
    assert!(res.fallbacks[0].from.contains("claude-fable-5"));
    // dead model is remembered
    assert!(health.is_dead(Provider::Claude, "claude-fable-5"));
}

#[tokio::test]
async fn all_rungs_fail_returns_error() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let ladder = vec![
        rung(
            Provider::Claude,
            "a",
            Arc::new(ScriptedAdapter::failing(
                Provider::Claude,
                "issue with the selected model (a). may not exist or you may not have access",
            )),
        ),
        rung(
            Provider::Claude,
            "b",
            Arc::new(ScriptedAdapter::failing(
                Provider::Claude,
                "issue with the selected model (b). may not exist or you may not have access",
            )),
        ),
    ];
    let err = run_with_failover(
        &ladder,
        "conductor",
        req,
        &store,
        &health,
        Duration::from_secs(30),
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("all 2 model rungs failed"));
    // Bail carries each rung's failure reason (not just the count).
    assert!(
        msg.contains("model unavailable"),
        "bail message should carry a rung failure reason, got: {msg}"
    );
}

#[tokio::test]
async fn dead_rung_is_skipped_on_reuse() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    health.mark_dead(Provider::Claude, "opus");
    let ladder = vec![
        rung(
            Provider::Claude,
            "opus",
            Arc::new(ScriptedAdapter::failing(Provider::Claude, "SHOULD NOT RUN")),
        ),
        rung(
            Provider::Claude,
            "sonnet",
            Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Claude,
                "via-sonnet",
            )),
        ),
    ];
    let res = run_with_failover(&ladder, "x", req, &store, &health, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(res.outcome.final_text, "via-sonnet");
    assert_eq!(res.rung_used, 1);
    // skipped-because-dead is still recorded as a fallback for transparency
    assert!(res
        .fallbacks
        .iter()
        .any(|f| f.reason.contains("known-dead")));
}

#[tokio::test]
async fn rate_limited_model_is_demoted_but_not_marked_dead() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let ladder = vec![
        rung(
            Provider::Claude,
            "opus",
            Arc::new(ScriptedAdapter::failing(
                Provider::Claude,
                "Claude usage limit reached; try again later",
            )),
        ),
        rung(
            Provider::Claude,
            "sonnet",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "recovered")),
        ),
    ];
    let res = run_with_failover(
        &ladder,
        "conductor",
        req,
        &store,
        &health,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert_eq!(res.rung_used, 1);
    assert_eq!(res.fallbacks.len(), 1);
    assert!(res.fallbacks[0].reason.contains("rate limited"));
    // LOAD-BEARING: a rate-limited model must demote but is NOT permanently
    // dead — a later use of the same model may succeed once the limit resets.
    assert!(!health.is_dead(Provider::Claude, "opus"));
}

#[tokio::test]
async fn spawn_error_demotes_to_next_rung() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let ladder = vec![
        rung(
            Provider::Claude,
            "missing-binary-model",
            Arc::new(SpawnFailAdapter::new(Provider::Claude)),
        ),
        rung(
            Provider::Claude,
            "sonnet",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "recovered")),
        ),
    ];
    let res = run_with_failover(
        &ladder,
        "conductor",
        req,
        &store,
        &health,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    // A launch failure on rung 0 demotes — it does NOT abort the ladder.
    assert_eq!(res.rung_used, 1);
    assert_eq!(res.outcome.final_text, "recovered");
    assert!(!res.fallbacks.is_empty());
    // A spawn error is local/transient, NOT evidence the model is dead.
    assert!(!health.is_dead(Provider::Claude, "missing-binary-model"));
}

#[tokio::test]
async fn transient_retry_recovers_on_same_rung() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let ladder = vec![rung(
        Provider::Claude,
        "opus",
        Arc::new(SequencedAdapter::new(
            Provider::Claude,
            vec![
                ScriptedAdapter::failing(Provider::Claude, "connection reset by peer"),
                ScriptedAdapter::ok_with_text(Provider::Claude, "recovered"),
            ],
        )),
    )];
    let res = run_with_failover(
        &ladder,
        "conductor",
        req,
        &store,
        &health,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    // Transient failure retried once on the SAME rung and recovered → no demotion.
    assert_eq!(res.rung_used, 0);
    assert!(res.fallbacks.is_empty());
    assert_eq!(res.outcome.final_text, "recovered");
    // model_used reflects the rung that produced the outcome.
    assert_eq!(res.model_used, "claude/opus");
}

// RateLimited now RETRIES on the same rung before demoting (previously it
// demoted immediately). With the default RetryConfig (one retry, zero backoff),
// a rung rate-limited once then recovering stays on rung 0 — the fix that keeps
// a briefly-throttled conductor from being abandoned. (claw-code-agent #3)
#[tokio::test]
async fn rate_limited_retries_same_rung_and_recovers() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let ladder = vec![rung(
        Provider::Claude,
        "opus",
        Arc::new(SequencedAdapter::new(
            Provider::Claude,
            vec![
                ScriptedAdapter::failing(
                    Provider::Claude,
                    "Claude usage limit reached; try again later",
                ),
                ScriptedAdapter::ok_with_text(Provider::Claude, "recovered"),
            ],
        )),
    )];
    let res = run_with_failover(
        &ladder,
        "conductor",
        req,
        &store,
        &health,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert_eq!(res.rung_used, 0, "recovered on the same rung, no demotion");
    assert!(res.fallbacks.is_empty());
    assert_eq!(res.outcome.final_text, "recovered");
}

// The prod-style RetryConfig sleeps an exponential backoff before each same-rung
// retry. With base 60ms + 2 retries the two backoffs (60ms + 120ms) mean the
// call cannot complete in under ~180ms — proving the backoff is actually applied
// (process-spawn time alone is far below this).
#[tokio::test]
async fn backoff_is_applied_before_each_retry() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::with_retry(RetryConfig {
        backoff_base: Duration::from_millis(60),
        max_retries: 2,
        cold_after: 0,
    });
    let ladder = vec![rung(
        Provider::Claude,
        "opus",
        Arc::new(SequencedAdapter::new(
            Provider::Claude,
            vec![
                ScriptedAdapter::failing(Provider::Claude, "connection reset by peer"),
                ScriptedAdapter::failing(Provider::Claude, "connection reset by peer"),
                ScriptedAdapter::ok_with_text(Provider::Claude, "recovered"),
            ],
        )),
    )];
    let start = std::time::Instant::now();
    let res = run_with_failover(
        &ladder,
        "conductor",
        req,
        &store,
        &health,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();
    assert_eq!(res.rung_used, 0);
    assert_eq!(res.outcome.final_text, "recovered");
    assert!(
        elapsed >= Duration::from_millis(180),
        "two backoffs (60ms + 120ms) must elapse; got {elapsed:?}"
    );
}

// Rate-limit circuit-breaker: after `cold_after` RateLimited rung-failures across
// the run, the rung is marked cold and skipped fast (without an attempt) for the
// rest of the run — so a throttled provider isn't hammered every subtask. It is
// NOT marked dead (rate-limit is recoverable). (claw-code-agent #3, deferred half)
#[tokio::test]
async fn rate_limit_circuit_breaker_trips_cold_after_threshold() {
    let store = QuotaStore::open_in_memory().unwrap();
    // cold_after=2, no same-rung retries → each run = exactly one rung-failure.
    let health = ModelHealth::with_retry(RetryConfig {
        backoff_base: Duration::ZERO,
        max_retries: 0,
        cold_after: 2,
    });
    let ladder = vec![
        rung(
            Provider::Claude,
            "opus",
            Arc::new(ScriptedAdapter::failing(
                Provider::Claude,
                "Claude usage limit reached; try again later",
            )),
        ),
        rung(
            Provider::Claude,
            "sonnet",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "recovered")),
        ),
    ];
    // Runs 1-2: opus rate-limits → demotes to sonnet; run 2 trips the breaker.
    for _ in 0..2 {
        let res = run_with_failover(
            &ladder,
            "conductor",
            req,
            &store,
            &health,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert_eq!(res.rung_used, 1);
    }
    assert!(
        health.is_cold(Provider::Claude, "opus"),
        "opus cold after 2 rate-limits"
    );
    assert!(
        !health.is_dead(Provider::Claude, "opus"),
        "rate-limit is not death"
    );
    // Run 3: opus is cold → skipped fast, fallback recorded with the cold reason.
    let res = run_with_failover(
        &ladder,
        "conductor",
        req,
        &store,
        &health,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert_eq!(res.rung_used, 1);
    assert!(res
        .fallbacks
        .iter()
        .any(|f| f.reason.contains("rate-limit-cold")));
}
