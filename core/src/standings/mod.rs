//! Standings: how each model has done, counted from the saved runs.
//!
//! This is a scoreboard for the person who chooses the lineup — which model
//! to make the reviewer, which worker to hand the hard subtask — and never
//! for a prompt. A model shown a ranking learns which number to raise, and
//! every number here that could be raised by saying more (findings reported,
//! ideas offered) is deliberately not counted. What is counted was decided by
//! someone else: the other council members' scores, a second model's ruling
//! on a finding, the project's own checks on a worker's change.

mod council;
mod names;
mod nominations;
mod render;
mod reviews;
mod source;
mod workers;

pub use render::render;
pub use source::load;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

/// Below this many results a rate says nothing, and no nomination is made.
pub const MIN_SAMPLE: u32 = 5;

/// Every table, most notable model first.
#[derive(Debug, Default, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Standings {
    /// How many saved runs were counted.
    pub runs: usize,
    /// Council answers, as the other members scored them.
    pub council: Vec<CouncilRow>,
    /// Review findings, as a second model ruled on them.
    pub reviewers: Vec<ReviewerRow>,
    /// The rulings each model gave as that second model.
    pub examiners: Vec<ExaminerRow>,
    /// Subtasks handed to each model as a worker.
    pub workers: Vec<WorkerRow>,
    /// Who stands out, where there is enough to go on.
    pub nominations: Vec<Nomination>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CouncilRow {
    pub model: String,
    /// Councils the model answered in.
    pub answers: u32,
    /// Answers that at least one other member scored.
    pub scored: u32,
    /// Mean score (1–10) the other members gave its answers; a member's
    /// score of its own answer is left out.
    pub peer_score: Option<f64>,
    /// Councils in which its answer was scored highest outright.
    pub best_answer: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ReviewerRow {
    pub model: String,
    pub reviews: u32,
    /// Findings a second model confirmed.
    pub confirmed: u32,
    /// Findings a second model refuted — false alarms.
    pub refuted: u32,
    pub unclear: u32,
    /// confirmed / (confirmed + refuted).
    pub held_up: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ExaminerRow {
    pub model: String,
    /// Reviews whose findings the model ruled on.
    pub examinations: u32,
    pub confirmed: u32,
    pub refuted: u32,
    pub unclear: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct WorkerRow {
    pub model: String,
    /// Subtasks the model was handed (`conduct` subtasks and `run_worker` calls).
    pub subtasks: u32,
    /// Those accepted on the first attempt, with the project's checks passing.
    pub first_try: u32,
    /// Extra attempts spent on its subtasks.
    pub reworks: u32,
    /// Attempts on which the project's checks failed.
    pub checks_failed: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Nomination {
    pub title: String,
    pub model: String,
    /// The numbers behind it.
    pub because: String,
}

/// Counts the standings from saved runs (each a transcript as saved under
/// `~/.consilium/runs`). Runs of a kind this does not know are counted as
/// runs and otherwise ignored.
pub fn tally(runs: &[Value]) -> Standings {
    let counted = Standings {
        runs: runs.len(),
        council: council::rows(runs),
        reviewers: reviews::reviewer_rows(runs),
        examiners: reviews::examiner_rows(runs),
        workers: workers::rows(runs),
        nominations: Vec::new(),
    };
    Standings {
        nominations: nominations::of(&counted),
        ..counted
    }
}

#[cfg(test)]
mod tests;
