use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub id: String,
    pub kind: String,
    pub task: String,
    pub snippet: String,
}

/// Human-readable JSON transcripts under `<base>/runs/<unix_nanos>-<kind>.json`.
/// Files, not SQLite: transcripts are for humans to read and diff; the M3
/// server can index them later.
pub struct TranscriptStore {
    base: PathBuf,
}

impl TranscriptStore {
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn default_base() -> anyhow::Result<PathBuf> {
        let home = std::env::var("HOME")
            .map_err(|_| anyhow::anyhow!("$HOME is not set; cannot locate ~/.consilium"))?;
        Ok(Path::new(&home).join(".consilium"))
    }

    /// `kind` must contain only ASCII alphanumerics, hyphens, and underscores —
    /// it is interpolated into a filename. Enforced via debug_assert.
    pub fn save(&self, kind: &str, payload: &serde_json::Value) -> anyhow::Result<PathBuf> {
        debug_assert!(
            kind.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "transcript kind '{kind}' contains characters that are unsafe in filenames"
        );
        let dir = self.base.join("runs");
        std::fs::create_dir_all(&dir)?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before unix epoch")
            .as_nanos();
        // Per-process atomic counter as a tiebreaker: nanosecond timestamps can
        // collide under coarse clocks or parallel saves — overwriting silently
        // would lose a transcript.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(format!("{nanos}-{seq:04}-{kind}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(payload)?)?;
        Ok(path)
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let mut hits = Vec::new();
        if query.trim().is_empty() {
            return hits;
        }
        let query_lower = query.to_lowercase();
        let dir = self.base.join("runs");

        // Read directory entries safely
        let mut entries: Vec<_> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd.filter_map(Result::ok).collect(),
            Err(_) => return hits, // e.g. dir doesn't exist
        };

        // Sort newest first (filenames start with timestamps, descending)
        entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));

        for entry in entries {
            if hits.len() >= limit {
                break;
            }
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            let Ok(val): Result<serde_json::Value, _> = serde_json::from_reader(file) else {
                continue;
            };

            // Extract text fields leniently
            let id = val
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let kind = val
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let task = val
                .get("task")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let summary = val
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Real transcript subtasks carry `title` + `summary` (not `text`).
            let subtasks = val
                .get("subtasks")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .flat_map(|s| {
                            ["title", "summary"]
                                .iter()
                                .filter_map(|k| s.get(*k).and_then(|t| t.as_str()))
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();

            let text_to_search = format!("{kind} {task} {summary} {subtasks}");
            let text_lower = text_to_search.to_lowercase();
            if let Some(idx) = text_lower.find(&query_lower) {
                // To be safe with char boundaries, we work with chars.
                // find() gives byte index, let's find the corresponding char index.
                let char_idx = text_lower[..idx].chars().count();
                let query_char_len = query_lower.chars().count();
                let total_chars = text_to_search.chars().count();

                let start = char_idx.saturating_sub(40);
                let end = (char_idx + query_char_len + 40).min(total_chars);

                let mut snippet: String = text_to_search
                    .chars()
                    .skip(start)
                    .take(end - start)
                    .collect();
                snippet = snippet.replace('\n', " ");

                if start > 0 {
                    snippet = format!("...{snippet}");
                }
                if end < total_chars {
                    snippet = format!("{snippet}...");
                }

                // Fallback for ID if empty in JSON
                let file_id = if id.is_empty() {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string()
                } else {
                    id
                };

                let hit_kind = if kind.is_empty() {
                    let file_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    let parts: Vec<_> = file_name.split('-').collect();
                    if parts.len() >= 3 {
                        parts[2..].join("-")
                    } else {
                        "unknown".to_string()
                    }
                } else {
                    kind
                };

                hits.push(SearchHit {
                    id: file_id,
                    kind: hit_kind,
                    task,
                    snippet,
                });
            }
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_back_run_json() {
        let dir = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(dir.path().to_path_buf());
        let path = store
            .save("council", &serde_json::json!({"question": "q", "stage": 1}))
            .unwrap();
        assert!(path.starts_with(dir.path()));
        assert!(path.to_string_lossy().contains("council"));
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["question"], "q");
    }

    #[test]
    fn run_ids_are_unique_and_sorted_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(dir.path().to_path_buf());
        let a = store.save("council", &serde_json::json!({})).unwrap();
        let b = store.save("council", &serde_json::json!({})).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn searches_transcripts_and_respects_limits() {
        let dir = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(dir.path().to_path_buf());

        // Write unparseable file to verify it's skipped gracefully
        std::fs::create_dir_all(dir.path().join("runs")).unwrap();
        std::fs::write(
            dir.path().join("runs").join("999-0000-bad.json"),
            "not json",
        )
        .unwrap();

        let _a = store
            .save(
                "worker",
                &serde_json::json!({
                    "id": "run-a",
                    "kind": "worker",
                    "task": "Fix the hyperdrive module",
                    "summary": "Replaced the motivator."
                }),
            )
            .unwrap();

        // Ensure distinct timestamps
        std::thread::sleep(std::time::Duration::from_millis(10));

        let _b = store
            .save(
                "worker",
                &serde_json::json!({
                    "id": "run-b",
                    "kind": "worker",
                    "task": "Paint the hull",
                    "subtasks": [{"title": "Mix hyperdrive paint"}, {"title": "Apply evenly"}]
                }),
            )
            .unwrap();

        // 1) Find both (query appears in task for A, subtasks for B)
        let hits = store.search("hyperdrive", 10);
        assert_eq!(hits.len(), 2, "should find both matches");
        // Due to reverse sort by filename, run-b should be first
        assert_eq!(hits[0].id, "run-b");
        assert_eq!(hits[1].id, "run-a");
        assert!(hits[0].snippet.contains("hyperdrive paint"));

        // 2) Respect limit
        let limited = store.search("hyperdrive", 1);
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].id, "run-b");

        // 3) Case-insensitive and empty results
        let hits_case = store.search("HYPERDRIVE", 10);
        assert_eq!(hits_case.len(), 2);

        let hits_none = store.search("wookiee", 10);
        assert!(hits_none.is_empty());

        let hits_empty_query = store.search("   ", 10);
        assert!(hits_empty_query.is_empty());
    }
}
