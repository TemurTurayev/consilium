mod commands;
mod fs;
mod git;
mod preflight;
mod trust;

pub use commands::{
    digest_commands, resolve_commands_with_provenance, CommandSource, VerificationCommand,
};
pub use fs::{ensure_owner_only_dir, write_owner_only_json};
pub use git::{
    create_detached_worktree, inspect_repository, remove_worktree, reopen_prepared_worktree,
    source_is_applyable, GitRepository, PreparedWorktree, PreparedWorktreeSummary, RepositoryKind,
    RepositoryState,
};
pub use preflight::{
    inspect, ExecutionMode, PreflightInput, ProviderReadiness, ReadinessState, RoleAssignment,
    SafetyPreflightReport,
};
pub use trust::{TrustKey, TrustStore};
