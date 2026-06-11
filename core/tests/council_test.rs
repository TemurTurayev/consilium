mod common;

use common::ScriptedAdapter;
use consilium::event::Provider;
use consilium::orchestrator::council::{run_council, CouncilMember};
use consilium::quota::QuotaStore;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn full_council_flow_with_scripted_members() {
    let store = QuotaStore::open_in_memory().unwrap();
    // Stage answers are plain text; stage-2 reviews return a JSON scores block;
    // ScriptedAdapter replays the same script for every call, so member answers
    // double as their review responses — parse_scores simply finds no JSON in
    // stage 2 for member 1 (None is tolerated by design).
    let members = vec![
        CouncilMember {
            label: "codex-worker".into(),
            adapter: Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "use sqlite")),
            model: None,
        },
        CouncilMember {
            label: "gemini-worker".into(),
            adapter: Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Gemini,
                "```json\n{\"scores\":[{\"agent\":\"A\",\"score\":7,\"justification\":\"ok\"}]}\n```",
            )),
            model: None,
        },
    ];
    let chairman = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        "final: use sqlite",
    ));

    let outcome = run_council(
        "which db?",
        members,
        chairman,
        None,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
    )
    .await
    .unwrap();

    assert_eq!(outcome.synthesis, "final: use sqlite");
    assert_eq!(outcome.answers.len(), 2);
    assert!(outcome.transcript["answers"].is_array());
    // Usage recorded for all stages: 2 answers + 2 reviews + 1 synthesis = 5 runs
    let (codex_in, _) = store.totals_since(Provider::Codex, 0).unwrap();
    assert!(codex_in > 0);
}

#[tokio::test]
async fn council_fails_when_all_members_fail() {
    let store = QuotaStore::open_in_memory().unwrap();
    let members = vec![CouncilMember {
        label: "w1".into(),
        adapter: Arc::new(ScriptedAdapter::failing(Provider::Codex, "quota exhausted")),
        model: None,
    }];
    let chairman = Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "unused"));
    let err = run_council(
        "q",
        members,
        chairman,
        None,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
    )
    .await
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("no council member produced an answer"));
}

#[tokio::test]
async fn council_proceeds_when_one_member_fails() {
    let store = QuotaStore::open_in_memory().unwrap();
    let members = vec![
        CouncilMember {
            label: "ok".into(),
            adapter: Arc::new(ScriptedAdapter::ok_with_text(Provider::Gemini, "answer")),
            model: None,
        },
        CouncilMember {
            label: "broken".into(),
            adapter: Arc::new(ScriptedAdapter::failing(Provider::Codex, "boom")),
            model: None,
        },
    ];
    let chairman = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Claude,
        "synthesized",
    ));
    let outcome = run_council(
        "q",
        members,
        chairman,
        None,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert_eq!(outcome.answers.len(), 1);
    assert_eq!(outcome.failed_members, vec!["broken".to_string()]);
}
