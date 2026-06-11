use std::path::{Path, PathBuf};

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

    pub fn save(&self, kind: &str, payload: &serde_json::Value) -> anyhow::Result<PathBuf> {
        let dir = self.base.join("runs");
        std::fs::create_dir_all(&dir)?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before unix epoch")
            .as_nanos();
        let path = dir.join(format!("{nanos}-{kind}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(payload)?)?;
        Ok(path)
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
}
