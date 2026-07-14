use crate::config::VerifyConfig;
use crate::orchestrator::verify::{command_timeout, detect_commands};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub enum CommandSource {
    AutoDetected,
    RepositoryConfig,
    UserProvided,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct VerificationCommand {
    pub label: String,
    pub command: String,
    pub source: CommandSource,
    #[ts(type = "number")]
    pub timeout_secs: u64,
}

pub fn digest_commands(commands: &[VerificationCommand]) -> String {
    let bytes = serde_json::to_vec(commands).expect("verification commands serialize");
    format!("{:x}", Sha256::digest(bytes))
}

pub fn resolve_commands_with_provenance(
    cwd: &Path,
    cfg: Option<&VerifyConfig>,
) -> Vec<VerificationCommand> {
    let timeout_secs = command_timeout(cfg).as_secs();
    let detected = detect_commands(cwd);
    let cfg = cfg.cloned().unwrap_or_default();

    [
        ("build", cfg.build.as_ref()),
        ("test", cfg.test.as_ref()),
        ("lint", cfg.lint.as_ref()),
    ]
    .into_iter()
    .filter_map(|(label, configured)| {
        configured
            .map(|command| (command.clone(), CommandSource::RepositoryConfig))
            .or_else(|| {
                detected
                    .iter()
                    .find(|(detected_label, _)| detected_label == label)
                    .map(|(_, command)| (command.clone(), CommandSource::AutoDetected))
            })
            .map(|(command, source)| VerificationCommand {
                label: label.into(),
                command,
                source,
                timeout_secs,
            })
    })
    .collect()
}
