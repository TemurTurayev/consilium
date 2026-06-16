mod common;

use common::ScriptedAdapter;
use consilium::adapters::{Adapter, RunRequest};
use consilium::config::ModelCandidate;
use consilium::event::Provider;
use consilium::orchestrator::resilience::{run_with_failover, ModelHealth, Rung};
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
    assert!(err.to_string().contains("all 2 model rungs failed"));
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
    let res = run_with_failover(
        &ladder,
        "x",
        req,
        &store,
        &health,
        Duration::from_secs(30),
    )
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
