//! Persistence for the desktop app's own settings (currently just the last
//! chosen workspace), stored at `~/.consilium/desktop.json`. Separate from
//! `consilium::config::Config`, which is the per-workspace council config.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesktopConfig {
    pub workspace: Option<String>,
}

/// `~/.consilium/desktop.json`. Falls back to `./.consilium/desktop.json`
/// only if the home directory can't be resolved (should not happen in
/// practice on macOS/Linux).
pub fn desktop_config_path() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".consilium").join("desktop.json")
}

/// `~/.consilium/usage.db` — the quota store shared across all workspaces.
pub fn usage_db_path() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".consilium").join("usage.db")
}

/// Read the desktop config; a missing or malformed file is treated as empty
/// (never fatal — this is best-effort UX state, not the council config).
pub fn load(path: &Path) -> DesktopConfig {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => DesktopConfig::default(),
    }
}

pub fn save(path: &Path, config: &DesktopConfig) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(config)?;
    std::fs::write(path, raw)?;
    Ok(())
}
