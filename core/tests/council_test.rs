mod common;

use common::ScriptedAdapter;
use consilium::config::ModelCandidate;
use consilium::event::Provider;
use consilium::orchestrator::council::{run_council, CouncilMember};
use consilium::orchestrator::resilience::{ModelHealth, Rung};
use consilium::quota::QuotaStore;
use std::sync::Arc;
use std::time::Duration;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn solo_member(
    label: &str,
    provider: Provider,
    model: &str,
    adapter: impl consilium::adapters::Adapter + 'static,
) -> CouncilMember {
    CouncilMember {
        label: label.into(),
        ladder: vec![Rung {
            candidate: ModelCandidate {
                provider,
                model: model.into(),
            },
            adapter: Arc::new(adapter),
        }],
    }
}

fn solo_ladder(
    provider: Provider,
    model: &str,
    adapter: impl consilium::adapters::Adapter + 'static,
) -> Vec<Rung> {
    vec![Rung {
        candidate: ModelCandidate {
            provider,
            model: model.into(),
        },
        adapter: Arc::new(adapter),
    }]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn full_council_flow_with_scripted_members() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    // Stage answers are plain text; stage-2 reviews return a JSON scores block;
    // ScriptedAdapter replays the same script for every call, so member answers
    // double as their review responses — parse_scores simply finds no JSON in
    // stage 2 for member 1 (None is tolerated by design).
    let members = vec![
        solo_member(
            "codex-worker",
            Provider::Codex,
            "gpt-4",
            ScriptedAdapter::ok_with_text(Provider::Codex, "use sqlite"),
        ),
        solo_member(
            "gemini-worker",
            Provider::Gemini,
            "gemini-pro",
            ScriptedAdapter::ok_with_text(
                Provider::Gemini,
                "```json\n{\"scores\":[{\"agent\":\"A\",\"score\":7,\"justification\":\"ok\"}]}\n```",
            ),
        ),
    ];
    let chairman = solo_ladder(
        Provider::Claude,
        "claude-opus",
        ScriptedAdapter::ok_with_text(Provider::Claude, "final: use sqlite"),
    );

    let outcome = run_council(
        "which db?",
        members,
        chairman,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
        &health,
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
async fn transcript_preserves_review_attribution() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    // Both members answer AND review with the same scripted text: "scorer"
    // emits a valid scores block (parsed), "prose" emits plain prose (null).
    let members = vec![
        solo_member(
            "scorer",
            Provider::Codex,
            "gpt-4",
            ScriptedAdapter::ok_with_text(
                Provider::Codex,
                "```json\n{\"scores\":[{\"agent\":\"A\",\"score\":7,\"justification\":\"fine\"}]}\n```",
            ),
        ),
        solo_member(
            "prose",
            Provider::Gemini,
            "gemini-pro",
            ScriptedAdapter::ok_with_text(
                Provider::Gemini,
                "I simply prefer the first answer, no numbers from me.",
            ),
        ),
    ];
    let chairman = solo_ladder(
        Provider::Claude,
        "claude-opus",
        ScriptedAdapter::ok_with_text(Provider::Claude, "done"),
    );

    let outcome = run_council(
        "q",
        members,
        chairman,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
        &health,
    )
    .await
    .unwrap();

    let scores = outcome.transcript["scores"].as_array().unwrap();
    assert_eq!(scores.len(), 2);
    let scorer_entry = scores
        .iter()
        .find(|e| e["member"] == "scorer")
        .expect("scorer entry present");
    assert!(!scorer_entry["parsed"].is_null());
    assert_eq!(scorer_entry["parsed"][0]["score"], 7);
    let prose_entry = scores
        .iter()
        .find(|e| e["member"] == "prose")
        .expect("prose entry present");
    assert!(prose_entry["parsed"].is_null());
    assert_eq!(outcome.transcript["reviews"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn council_fails_when_all_members_fail() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();
    let members = vec![solo_member(
        "w1",
        Provider::Codex,
        "gpt-4",
        ScriptedAdapter::failing(Provider::Codex, "quota exhausted"),
    )];
    let chairman = solo_ladder(
        Provider::Claude,
        "claude-opus",
        ScriptedAdapter::ok_with_text(Provider::Claude, "unused"),
    );
    let err = run_council(
        "q",
        members,
        chairman,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
        &health,
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
    let health = ModelHealth::new();
    let members = vec![
        solo_member(
            "ok",
            Provider::Gemini,
            "gemini-pro",
            ScriptedAdapter::ok_with_text(Provider::Gemini, "answer"),
        ),
        solo_member(
            "broken",
            Provider::Codex,
            "gpt-4",
            ScriptedAdapter::failing(Provider::Codex, "boom"),
        ),
    ];
    let chairman = solo_ladder(
        Provider::Claude,
        "claude-opus",
        ScriptedAdapter::ok_with_text(Provider::Claude, "synthesized"),
    );
    let outcome = run_council(
        "q",
        members,
        chairman,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
        &health,
    )
    .await
    .unwrap();
    assert_eq!(outcome.answers.len(), 1);
    assert_eq!(outcome.failed_members, vec!["broken".to_string()]);
}

#[tokio::test]
async fn council_member_falls_back_to_second_model() {
    let store = QuotaStore::open_in_memory().unwrap();
    let health = ModelHealth::new();

    // Member has a two-rung ladder: first rung fails with model-unavailable,
    // second rung succeeds.
    let failing_rung = Rung {
        candidate: ModelCandidate {
            provider: Provider::Claude,
            model: "claude-fable-5".into(),
        },
        adapter: Arc::new(ScriptedAdapter::failing(
            Provider::Claude,
            "There's an issue with the selected model (claude-fable-5). It may not exist or you may not have access to it.",
        )),
    };
    let ok_rung = Rung {
        candidate: ModelCandidate {
            provider: Provider::Claude,
            model: "claude-opus-4-8".into(),
        },
        adapter: Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Claude,
            "fallback answer",
        )),
    };

    let members = vec![CouncilMember {
        label: "worker".into(),
        ladder: vec![failing_rung, ok_rung],
    }];

    let chairman = solo_ladder(
        Provider::Claude,
        "claude-opus",
        ScriptedAdapter::ok_with_text(Provider::Claude, "synthesis done"),
    );

    let outcome = run_council(
        "test question",
        members,
        chairman,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
        &health,
    )
    .await
    .unwrap();

    assert_eq!(outcome.synthesis, "synthesis done");
    // The fallback answer made it into the council
    assert!(outcome
        .answers
        .iter()
        .any(|(_, _, text)| text == "fallback answer"));

    // Transcript must have a non-empty fallbacks array
    let fallbacks = outcome.transcript["fallbacks"].as_array().unwrap();
    assert!(
        !fallbacks.is_empty(),
        "transcript fallbacks must be non-empty when a member fell back"
    );
    // The fallback records the demotion from the dead model
    assert!(
        fallbacks
            .iter()
            .any(|fb| fb["from"].as_str().unwrap_or("").contains("claude-fable-5")),
        "fallbacks should record the failed model"
    );
}
