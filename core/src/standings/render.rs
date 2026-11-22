//! The standings as text, for a terminal.

use super::Standings;

/// How wide the model column is.
const MODEL_WIDTH: usize = 34;

pub fn render(standings: &Standings) -> String {
    if standings.runs == 0 {
        return "standings — no saved runs yet\n".to_string();
    }
    let sections = [
        council(standings),
        reviewers(standings),
        examiners(standings),
        workers(standings),
        nominations(standings),
    ];
    let body: Vec<String> = sections.into_iter().flatten().collect();
    format!(
        "standings — from {} saved runs\n\n{}",
        standings.runs,
        body.join("\n")
    )
}

/// A titled table, or nothing when it has no rows.
fn section(title: &str, header: &[&str], rows: Vec<(String, Vec<String>)>) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let line = |model: &str, cells: Vec<String>| {
        let cells: String = cells.iter().map(|cell| format!("  {cell:>10}")).collect();
        format!("  {model:<MODEL_WIDTH$}{cells}\n")
    };
    let head = line("model", header.iter().map(|h| h.to_string()).collect());
    let body: String = rows
        .into_iter()
        .map(|(model, cells)| line(&model, cells))
        .collect();
    Some(format!("{title}\n{head}{body}"))
}

fn council(standings: &Standings) -> Option<String> {
    section(
        "council answers — scored 1–10 by the other members (never by their own author);\n\
         own words: how much of what it said nobody else said; kept: how much of that the synthesis used",
        &["answers", "scored", "peer score", "best", "own words", "kept"],
        standings
            .council
            .iter()
            .map(|row| {
                let score = row
                    .peer_score
                    .map_or("–".to_string(), |score| format!("{score:.1}"));
                let cells = vec![
                    row.answers.to_string(),
                    row.scored.to_string(),
                    score,
                    row.best_answer.to_string(),
                    row.own_words.map_or("–".to_string(), percent),
                    row.kept.map_or("–".to_string(), percent),
                ];
                (row.model.clone(), cells)
            })
            .collect(),
    )
}

fn reviewers(standings: &Standings) -> Option<String> {
    section(
        "review findings — as a second model ruled on them",
        &["reviews", "confirmed", "refuted", "unclear", "held up"],
        standings
            .reviewers
            .iter()
            .map(|row| {
                let cells = vec![
                    row.reviews.to_string(),
                    row.confirmed.to_string(),
                    row.refuted.to_string(),
                    row.unclear.to_string(),
                    row.held_up.map_or("–".to_string(), percent),
                ];
                (row.model.clone(), cells)
            })
            .collect(),
    )
}

fn examiners(standings: &Standings) -> Option<String> {
    section(
        "rulings given — as the second model",
        &["rulings on", "confirmed", "refuted", "unclear"],
        standings
            .examiners
            .iter()
            .map(|row| {
                let cells = vec![
                    row.examinations.to_string(),
                    row.confirmed.to_string(),
                    row.refuted.to_string(),
                    row.unclear.to_string(),
                ];
                (row.model.clone(), cells)
            })
            .collect(),
    )
}

fn workers(standings: &Standings) -> Option<String> {
    section(
        "workers — subtasks handed to each model",
        &["subtasks", "first try", "reworks", "checks ✗"],
        standings
            .workers
            .iter()
            .map(|row| {
                let cells = vec![
                    row.subtasks.to_string(),
                    row.first_try.to_string(),
                    row.reworks.to_string(),
                    row.checks_failed.to_string(),
                ];
                (row.model.clone(), cells)
            })
            .collect(),
    )
}

fn nominations(standings: &Standings) -> Option<String> {
    if standings.nominations.is_empty() {
        return None;
    }
    let lines: String = standings
        .nominations
        .iter()
        .map(|n| format!("  {}: {} — {}\n", n.title, n.model, n.because))
        .collect();
    Some(format!(
        "nominations — for choosing the lineup; never put them in a prompt\n{lines}"
    ))
}

fn percent(share: f64) -> String {
    format!("{:.0}%", share * 100.0)
}
