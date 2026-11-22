//! Reading the saved runs.

use serde_json::Value;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Every saved run under `base` (`<base>/runs/*.json`) that is no older than
/// `within`, oldest first. A run's age is read from its file name, which
/// starts with the time it was saved. Files that do not parse are skipped.
pub fn load(base: &Path, within: Option<Duration>) -> Vec<Value> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_nanos());
    let earliest = within.map_or(0, |within| now.saturating_sub(within.as_nanos()));
    let Ok(entries) = std::fs::read_dir(base.join("runs")) else {
        return Vec::new();
    };
    let mut saved: Vec<(u128, std::path::PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| Some((saved_at(&path)?, path)))
        .filter(|(saved_at, _)| *saved_at >= earliest)
        .collect();
    saved.sort();
    saved
        .into_iter()
        .filter_map(|(_, path)| std::fs::read_to_string(path).ok())
        .filter_map(|raw| serde_json::from_str(&raw).ok())
        .collect()
}

/// When a run was saved, in nanoseconds since the epoch: the number its file
/// name starts with (`<nanos>-<seq>-<kind>.json`).
fn saved_at(path: &Path) -> Option<u128> {
    path.file_name()?.to_str()?.split('-').next()?.parse().ok()
}
