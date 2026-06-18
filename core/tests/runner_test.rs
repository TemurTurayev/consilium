mod common;

use common::ScriptedAdapter;
use consilium::adapters::RunRequest;
use consilium::event::{AgentEvent, Provider};
use consilium::orchestrator::progress::{ProgressSink, PROGRESS_SINK};
use consilium::orchestrator::runner::{run_to_completion, RunStatus};
use consilium::quota::QuotaStore;
use std::sync::{Arc, Mutex};
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

/// A scoped ProgressSink receives every event the run collects; with no sink in
/// scope the run behaves exactly as before (covered by every other test).
#[tokio::test]
async fn progress_sink_in_scope_receives_every_event() {
    struct VecSink(Arc<Mutex<Vec<AgentEvent>>>);
    impl ProgressSink for VecSink {
        fn on_event(&self, ev: &AgentEvent) {
            self.0.lock().unwrap().push(ev.clone());
        }
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink: Arc<dyn ProgressSink> = Arc::new(VecSink(seen.clone()));

    let store = QuotaStore::open_in_memory().unwrap();
    let adapter = Arc::new(ScriptedAdapter::ok_with_text(Provider::Gemini, "streamed"));

    let outcome = PROGRESS_SINK
        .scope(sink, async {
            run_to_completion(adapter, req(), &store, Duration::from_secs(30)).await
        })
        .await
        .unwrap();

    assert!(matches!(outcome.status, RunStatus::Completed));
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "sink must have received events live");
    assert_eq!(
        seen.len(),
        outcome.events.len(),
        "sink should see exactly the events the run collected"
    );
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

// A timed-out run must KILL the child, not orphan it: a worker that would write a
// marker only AFTER a sleep longer than the timeout must never produce it,
// because kill_on_drop SIGKILLs the child when the reader task is aborted.
#[tokio::test]
async fn timeout_kills_child_so_it_cannot_keep_writing() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("late.txt");
    let adapter = Arc::new(ScriptedAdapter {
        provider: Provider::Gemini,
        script: String::new(),
        delay_secs: 0,
        // Runs in the child's cwd; writes the marker only after a 1s sleep.
        pre_script: "sleep 1; echo late > late.txt".into(),
    });
    let store = QuotaStore::open_in_memory().unwrap();
    let req = RunRequest {
        prompt: "q".into(),
        model: None,
        cwd: dir.path().to_path_buf(),
        advisory: false,
        write: false,
    };

    let outcome = run_to_completion(adapter, req, &store, Duration::from_millis(200))
        .await
        .unwrap();
    assert!(matches!(outcome.status, RunStatus::TimedOut));

    // Well past the child's 1s sleep: an orphaned child would have written the
    // marker by now; a killed one never will.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !marker.exists(),
        "timed-out child must be SIGKILLed before it can write the marker"
    );

    // Positive control: the SAME script, given time to finish, DOES write the
    // marker — proving the suppression above is the kill, not a missing write.
    let dir2 = tempfile::tempdir().unwrap();
    let marker2 = dir2.path().join("late.txt");
    let adapter2 = Arc::new(ScriptedAdapter {
        provider: Provider::Gemini,
        script: String::new(),
        delay_secs: 0,
        pre_script: "sleep 1; echo late > late.txt".into(),
    });
    let req2 = RunRequest {
        prompt: "q".into(),
        model: None,
        cwd: dir2.path().to_path_buf(),
        advisory: false,
        write: false,
    };
    let _ = run_to_completion(adapter2, req2, &store, Duration::from_secs(10))
        .await
        .unwrap();
    assert!(
        marker2.exists(),
        "an un-killed run completes its write (positive control)"
    );
}
