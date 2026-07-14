use consilium::config::VerifyConfig;
use consilium::safety::{
    digest_commands, inspect, resolve_commands_with_provenance, CommandSource, ExecutionMode,
    PreflightInput, RepositoryKind, VerificationCommand,
};
use tempfile::tempdir;

#[test]
fn standalone_git_write_defaults_to_safe_worktree() {
    let dir = tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(dir.path())
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "base",
        ])
        .current_dir(dir.path())
        .status()
        .unwrap();

    let report = inspect(PreflightInput::standalone(dir.path().to_path_buf(), None)).unwrap();
    assert_eq!(report.repository.kind, RepositoryKind::Git);
    assert_eq!(report.default_mode, ExecutionMode::SafeWorktree);
    assert_eq!(report.commands[0].source, CommandSource::AutoDetected);
    assert!(!report.provider_probe_performed);
}

#[test]
fn non_git_write_has_no_fake_safe_default() {
    let dir = tempdir().unwrap();
    let report = inspect(PreflightInput::standalone(dir.path().to_path_buf(), None)).unwrap();
    assert_eq!(report.repository.kind, RepositoryKind::NonGit);
    assert_eq!(report.default_mode, ExecutionMode::ReadOnly);
    assert!(report.available_modes.contains(&ExecutionMode::InPlace));
    assert!(!report
        .available_modes
        .contains(&ExecutionMode::SafeWorktree));
}

#[test]
fn configured_commands_preserve_per_field_provenance_and_timeout() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname='fixture'\n").unwrap();
    let cfg = VerifyConfig {
        test: Some("cargo test --workspace".into()),
        timeout_secs: Some(42),
        ..Default::default()
    };

    let commands = resolve_commands_with_provenance(dir.path(), Some(&cfg));
    let build = commands.iter().find(|item| item.label == "build").unwrap();
    let test = commands.iter().find(|item| item.label == "test").unwrap();

    assert_eq!(build.source, CommandSource::AutoDetected);
    assert_eq!(test.source, CommandSource::RepositoryConfig);
    assert_eq!(test.command, "cargo test --workspace");
    assert!(commands.iter().all(|item| item.timeout_secs == 42));
}

#[test]
fn safety_enums_serialize_as_snake_case() {
    assert_eq!(
        serde_json::to_string(&ExecutionMode::SafeWorktree).unwrap(),
        r#""safe_worktree""#
    );
    assert_eq!(
        serde_json::to_string(&CommandSource::UserProvided).unwrap(),
        r#""user_provided""#
    );
}

#[test]
fn command_digest_is_stable_sha256_and_changes_with_command() {
    let commands = vec![VerificationCommand {
        label: "test".into(),
        command: "cargo test".into(),
        source: CommandSource::RepositoryConfig,
        timeout_secs: 600,
    }];
    let same = commands.clone();
    let changed = vec![VerificationCommand {
        command: "cargo test --workspace".into(),
        ..commands[0].clone()
    }];

    let digest = digest_commands(&commands);
    assert_eq!(digest, digest_commands(&same));
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(digest, digest_commands(&changed));
}
