use consilium::config::VerifyConfig;
use consilium::safety::{
    digest_commands, inspect, resolve_commands_with_provenance, CommandSource, ExecutionMode,
    PreflightInput, RepositoryKind, SafetyPreflightReport, VerificationCommand,
};
use tempfile::tempdir;
use ts_rs::{Config as TsConfig, TS};

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
    let warning = report.warnings.join(" ");
    assert!(warning.contains("read-only"), "{warning}");
    assert!(warning.contains("initialize Git"), "{warning}");
    assert!(warning.contains("explicit in-place"), "{warning}");
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

#[test]
fn safety_bindings_target_workspace_and_use_js_numbers() {
    assert_eq!(
        <ExecutionMode as TS>::output_path().unwrap(),
        std::path::PathBuf::from("../../ui/src/protocol/ExecutionMode.ts")
    );

    let ts_config = TsConfig::default();
    let command = VerificationCommand::export_to_string(&ts_config).unwrap();
    assert!(command.contains("timeout_secs: number"), "{command}");
    assert!(!command.contains("bigint"), "{command}");

    let report = SafetyPreflightReport::export_to_string(&ts_config).unwrap();
    assert!(report.contains("timeout_secs: number"), "{report}");
    assert!(report.contains("budget_secs: number | null"), "{report}");
    assert!(!report.contains("bigint"), "{report}");
}

#[test]
fn attached_inspection_accepts_paths_inside_launch_root_and_discloses_acknowledgement() {
    let root = tempdir().unwrap();
    let child = root.path().join("child");
    std::fs::create_dir(&child).unwrap();

    let report = inspect(PreflightInput::attached(
        child,
        root.path().to_path_buf(),
        None,
        Vec::new(),
    ))
    .unwrap();

    assert_eq!(report.default_mode, ExecutionMode::InPlace);
    assert!(report.warnings.iter().any(|warning| {
        warning.contains("in-place") && warning.contains("explicit acknowledgement")
    }));
}

#[test]
fn attached_inspection_rejects_paths_outside_launch_root() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();

    let error = inspect(PreflightInput::attached(
        outside.path().to_path_buf(),
        root.path().to_path_buf(),
        None,
        Vec::new(),
    ))
    .unwrap_err();

    assert!(error.to_string().contains("outside launch root"), "{error}");
}

#[cfg(unix)]
#[test]
fn attached_inspection_rejects_symlink_escape() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let link = root.path().join("escape");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();

    let error = inspect(PreflightInput::attached(
        link,
        root.path().to_path_buf(),
        None,
        Vec::new(),
    ))
    .unwrap_err();

    assert!(error.to_string().contains("outside launch root"), "{error}");
}

#[test]
fn git_repository_without_head_does_not_offer_safe_worktree() {
    let dir = tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();

    let report = inspect(PreflightInput::standalone(dir.path().to_path_buf(), None)).unwrap();

    assert_eq!(report.repository.kind, RepositoryKind::Git);
    assert_eq!(report.repository.head, None);
    assert_eq!(report.default_mode, ExecutionMode::ReadOnly);
    assert_eq!(
        report.available_modes,
        vec![ExecutionMode::InPlace, ExecutionMode::ReadOnly]
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("first commit")),
        "{:?}",
        report.warnings
    );
}

#[test]
fn git_root_ending_in_whitespace_is_preserved() {
    let parent = tempdir().unwrap();
    let repo = parent.path().join("repo ");
    std::fs::create_dir(&repo).unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap();

    let report = inspect(PreflightInput::standalone(repo.clone(), None)).unwrap();

    let expected = repo.canonicalize().unwrap().display().to_string();
    assert_eq!(
        report.repository.git_root.as_deref(),
        Some(expected.as_str())
    );
}
