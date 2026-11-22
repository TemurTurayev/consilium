//! Subtasks handed to each model as a worker.

use super::names::{is_kind, list, model_name, text};
use super::WorkerRow;
use serde_json::Value;
use std::collections::BTreeMap;

/// One subtask as it went for the model it was handed to.
struct Subtask {
    model: String,
    first_try: bool,
    reworks: u32,
    checks_failed: u32,
}

pub(super) fn rows(runs: &[Value]) -> Vec<WorkerRow> {
    let conducted = runs
        .iter()
        .filter(|run| is_kind(run, "conduct"))
        .flat_map(|run| list(run, "subtasks"))
        .filter_map(conducted_subtask);
    let delegated = runs
        .iter()
        .filter(|run| is_kind(run, "worker"))
        .filter_map(delegated_run);
    let by_model = conducted.chain(delegated).fold(
        BTreeMap::<String, WorkerRow>::new(),
        |mut by_model, subtask| {
            let so_far = by_model.get(&subtask.model).cloned().unwrap_or(WorkerRow {
                model: subtask.model.clone(),
                subtasks: 0,
                first_try: 0,
                reworks: 0,
                checks_failed: 0,
            });
            by_model.insert(
                subtask.model.clone(),
                WorkerRow {
                    subtasks: so_far.subtasks + 1,
                    first_try: so_far.first_try + u32::from(subtask.first_try),
                    reworks: so_far.reworks + subtask.reworks,
                    checks_failed: so_far.checks_failed + subtask.checks_failed,
                    ..so_far
                },
            );
            by_model
        },
    );
    let mut rows: Vec<WorkerRow> = by_model.into_values().collect();
    rows.sort_by(|a, b| b.subtasks.cmp(&a.subtasks).then(a.model.cmp(&b.model)));
    rows
}

/// A `conduct` subtask, counted for the worker that took it first. One that
/// no worker attempted (skipped, or halted before it began) is not counted.
fn conducted_subtask(subtask: &Value) -> Option<Subtask> {
    let attempts = list(subtask, "attempts");
    let worker = text(attempts.first()?, "worker")?;
    let checks_failed = attempts
        .iter()
        .filter(|attempt| text(attempt, "verify") == Some("failed"))
        .count();
    Some(Subtask {
        model: model_name(None, worker),
        first_try: text(subtask, "status") == Some("completed")
            && attempts.len() == 1
            && checks_failed == 0,
        reworks: count(attempts.len() - 1),
        checks_failed: count(checks_failed),
    })
}

/// A `run_worker` call: one subtask, one attempt.
fn delegated_run(run: &Value) -> Option<Subtask> {
    let worker = text(run, "worker")?;
    let checks_failed = run["verify"]["passed"].as_bool() == Some(false);
    Some(Subtask {
        model: model_name(text(run, "model_used"), worker),
        first_try: run.get("failed").is_none() && !checks_failed,
        reworks: 0,
        checks_failed: u32::from(checks_failed),
    })
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
