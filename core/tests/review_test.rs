mod common;

use common::ScriptedAdapter;
use consilium::event::Provider;
use consilium::orchestrator::review::run_review;
use consilium::quota::QuotaStore;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn reviews_a_diff_and_returns_verdict() {
    let store = QuotaStore::open_in_memory().unwrap();
    let reviewer = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Codex,
        "```json\n{\"findings\":[{\"severity\":\"important\",\"file\":\"main.rs\",\"description\":\"unwrap on user input\"}]}\n```",
    ));
    let result = run_review(
        "--- a/main.rs\n+++ b/main.rs\n+let x = input.unwrap();",
        reviewer,
        None,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    let verdict = result.verdict.expect("verdict parsed");
    assert_eq!(verdict.findings.len(), 1);
    assert!(!verdict.has_critical());
    assert!(result.transcript["raw_review"].is_string());
    assert_eq!(result.transcript["parse_ok"], true);
    assert!(result.transcript["diff_preview"]
        .as_str()
        .unwrap()
        .contains("+let x = input.unwrap();"));
}

#[tokio::test]
async fn unparseable_review_still_returns_raw_text() {
    let store = QuotaStore::open_in_memory().unwrap();
    let reviewer = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Gemini,
        "LGTM, ship it",
    ));
    let result = run_review(
        "diff",
        reviewer,
        None,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert!(result.verdict.is_none());
    assert_eq!(result.raw_review, "LGTM, ship it");
}

#[tokio::test]
async fn review_fails_when_reviewer_fails() {
    let store = QuotaStore::open_in_memory().unwrap();
    let reviewer = Arc::new(ScriptedAdapter::failing(Provider::Codex, "quota exhausted"));
    let err = run_review(
        "diff",
        reviewer,
        None,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("reviewer failed"));
}
