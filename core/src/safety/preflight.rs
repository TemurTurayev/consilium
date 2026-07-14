use super::{
    digest_commands, inspect_repository, resolve_commands_with_provenance, RepositoryKind,
    RepositoryState, VerificationCommand,
};
use crate::config::{Config, RoleConfig};
use crate::confine::cwd_within_root;
use crate::orchestrator::verify::command_timeout;
use crate::protocol::ConfigSummary;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub enum ExecutionMode {
    SafeWorktree,
    InPlace,
    ReadOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct RoleAssignment {
    pub role: String,
    pub primary: String,
    pub fallbacks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub enum ReadinessState {
    UnknownNotProbed,
    Ready,
    NeedsLogin,
    CliMissing,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct ProviderReadiness {
    pub provider: String,
    pub state: ReadinessState,
    pub detail: String,
    pub hint: String,
    pub probed: bool,
}

#[derive(Debug, Clone)]
pub struct PreflightInput {
    pub cwd: PathBuf,
    pub config: Option<Config>,
    pub provider_readiness: Vec<ProviderReadiness>,
    attached: bool,
    confinement_root: Option<PathBuf>,
}

impl PreflightInput {
    pub fn standalone(cwd: PathBuf, config: Option<Config>) -> Self {
        Self {
            cwd,
            config,
            provider_readiness: Vec::new(),
            attached: false,
            confinement_root: None,
        }
    }

    pub fn attached(
        cwd: PathBuf,
        launch_root: PathBuf,
        config: Option<Config>,
        provider_readiness: Vec<ProviderReadiness>,
    ) -> Self {
        Self {
            cwd,
            config,
            provider_readiness,
            attached: true,
            confinement_root: Some(launch_root),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct SafetyPreflightReport {
    pub repository: RepositoryState,
    pub default_mode: ExecutionMode,
    pub available_modes: Vec<ExecutionMode>,
    pub commands: Vec<VerificationCommand>,
    pub command_digest: String,
    pub roles: Vec<RoleAssignment>,
    pub provider_readiness: Vec<ProviderReadiness>,
    #[ts(type = "number")]
    pub timeout_secs: u64,
    #[ts(type = "number | null")]
    pub budget_secs: Option<u64>,
    pub provider_probe_performed: bool,
    pub warnings: Vec<String>,
}

pub fn inspect(input: PreflightInput) -> Result<SafetyPreflightReport> {
    if let Some(root) = input.confinement_root.as_deref() {
        if !cwd_within_root(&input.cwd, root) {
            anyhow::bail!(
                "attached inspection path {} is outside launch root {}",
                input.cwd.display(),
                root.display()
            );
        }
    }
    let canonical_cwd = input
        .cwd
        .canonicalize()
        .with_context(|| format!("canonicalize {}", input.cwd.display()))?;
    let repository = inspect_repository(&canonical_cwd)?;
    let config = input.config.unwrap_or_default();
    let commands = resolve_commands_with_provenance(&canonical_cwd, config.verify.as_ref());
    let command_digest = digest_commands(&commands);
    let timeout_secs = command_timeout(config.verify.as_ref()).as_secs();
    let roles = role_assignments(&config);
    let provider_probe_performed = input.provider_readiness.iter().any(|item| item.probed);

    let (default_mode, available_modes) =
        match (repository.kind, repository.head.is_some(), input.attached) {
            (_, _, true) => (
                ExecutionMode::InPlace,
                vec![ExecutionMode::InPlace, ExecutionMode::ReadOnly],
            ),
            (RepositoryKind::Git, true, false) => (
                ExecutionMode::SafeWorktree,
                vec![
                    ExecutionMode::SafeWorktree,
                    ExecutionMode::InPlace,
                    ExecutionMode::ReadOnly,
                ],
            ),
            (_, _, false) => (
                ExecutionMode::ReadOnly,
                vec![ExecutionMode::InPlace, ExecutionMode::ReadOnly],
            ),
        };

    let mut warnings = Vec::new();
    warnings.push("in-place execution requires explicit acknowledgement before execution.".into());
    if repository.kind == RepositoryKind::NonGit {
        warnings.push(
            "Safe worktree isolation is unavailable outside a Git repository; choose a read-only action, initialize Git, or use explicit in-place execution."
                .into(),
        );
    }
    if repository.kind == RepositoryKind::Git && repository.head.is_none() {
        warnings.push(
            "Create the first commit before choosing safe worktree isolation; until then use a read-only action or explicit in-place execution."
                .into(),
        );
    }
    if !repository.clean {
        warnings.push(
            "The source checkout is dirty; safe worktree runs use committed HEAD and apply remains disabled until it is clean."
                .into(),
        );
    }
    if input.attached {
        warnings.push(
            "Attached mode inherits the host permission model and executes writes in-place.".into(),
        );
    }

    Ok(SafetyPreflightReport {
        repository,
        default_mode,
        available_modes,
        commands,
        command_digest,
        roles,
        provider_readiness: input.provider_readiness,
        timeout_secs,
        budget_secs: config.budget_secs,
        provider_probe_performed,
        warnings,
    })
}

fn role_assignments(config: &Config) -> Vec<RoleAssignment> {
    let summary = ConfigSummary::from_config(config, None);
    let roles = &config.roles;
    let mut assignments = vec![assignment("conductor", summary.conductor, &roles.conductor)];
    assignments.extend(roles.workers.iter().enumerate().map(|(index, role)| {
        assignment(
            &format!("worker_{}", index + 1),
            summary.workers[index].clone(),
            role,
        )
    }));
    assignments.extend([
        assignment("reviewer", summary.reviewer, &roles.reviewer),
        assignment("chairman", summary.chairman, &roles.chairman),
        assignment("supervisor", summary.supervisor, &roles.supervisor),
    ]);
    assignments
}

fn assignment(role: &str, primary: String, config: &RoleConfig) -> RoleAssignment {
    RoleAssignment {
        role: role.into(),
        primary,
        fallbacks: config
            .fallbacks
            .iter()
            .map(|item| format!("{}/{}", item.provider.as_str(), item.model))
            .collect(),
    }
}
