mod common;

use common::ScriptedAdapter;
use consilium::adapters::RunRequest;
use consilium::event::Provider;
use consilium::orchestrator::runner::{run_to_completion, RunStatus};
use consilium::quota::QuotaStore;
use std::sync::Arc;
use std::time::Duration;

fn req() -> RunRequest {
    RunRequest {
        prompt: "q".into(),
        model: None,
        cwd: std::env::temp_dir(),
        advisory: false,
        write: false,
    }
}

#[tokio::test]
async fn collects_final_text_and_records_usage() {
    let store = QuotaStore::open_in_memory().unwrap();
    let adapter = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Gemini,
        "the answer",
    ));
    let outcome = run_to_completion(adapter, req(), &store, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(outcome.final_text, "the answer");
    assert!(matches!(outcome.status, RunStatus::Completed));
    let (input, output) = store.totals_since(Provider::Gemini, 0).unwrap();
    assert_eq!((input, output), (10, 5));
}

#[tokio::test]
async fn failed_event_yields_failed_status() {
    let store = QuotaStore::open_in_memory().unwrap();
    let adapter = Arc::new(ScriptedAdapter::failing(Provider::Codex, "limit reached"));
    let outcome = run_to_completion(adapter, req(), &store, Duration::from_secs(30))
        .await
        .unwrap();
    assert!(matches!(&outcome.status, RunStatus::Failed(e) if e.contains("limit reached")));
}

#[tokio::test]
async fn timeout_yields_timedout_status() {
    let store = QuotaStore::open_in_memory().unwrap();
    let adapter = Arc::new(ScriptedAdapter {
        provider: Provider::Gemini,
        script: String::new(),
        delay_secs: 30,
        pre_script: String::new(),
    });
    let outcome = run_to_completion(adapter, req(), &store, Duration::from_millis(200))
        .await
        .unwrap();
    assert!(matches!(outcome.status, RunStatus::TimedOut));
    assert!(outcome.events.is_empty());
    assert!(outcome.final_text.is_empty());
}
