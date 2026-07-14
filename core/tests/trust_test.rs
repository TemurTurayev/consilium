use consilium::safety::{digest_commands, CommandSource, TrustStore, VerificationCommand};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::tempdir;

fn repository_command(command: &str) -> VerificationCommand {
    VerificationCommand {
        label: "test".into(),
        command: command.into(),
        source: CommandSource::RepositoryConfig,
        timeout_secs: 600,
    }
}

fn create_repo(root: &Path) -> std::path::PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    repo
}

#[test]
fn changing_a_repository_command_invalidates_trust() {
    let root = tempdir().unwrap();
    let repo = create_repo(root.path());
    let state = root.path().join("state");
    let store = TrustStore::open(state.clone()).unwrap();
    let first = vec![repository_command("cargo test")];

    store.trust(&repo, &first).unwrap();
    assert!(store.is_trusted(&repo, &first).unwrap());

    let reopened = TrustStore::open(state).unwrap();
    assert!(reopened.is_trusted(&repo, &first).unwrap());

    let changed = vec![repository_command("cargo test --release")];
    assert_ne!(digest_commands(&first), digest_commands(&changed));
    assert!(!reopened.is_trusted(&repo, &changed).unwrap());
}

#[cfg(unix)]
#[test]
fn trust_state_is_owner_only() {
    let root = tempdir().unwrap();
    let store = TrustStore::open(root.path().join("state")).unwrap();

    store.trust(root.path(), &[]).unwrap();

    assert_eq!(
        fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(store.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn corrupt_trust_state_fails_closed_without_replacing_it() {
    let root = tempdir().unwrap();
    let repo = create_repo(root.path());
    let store = TrustStore::open(root.path().join("state")).unwrap();
    fs::write(store.path(), b"not valid json").unwrap();
    let commands = vec![repository_command("cargo test")];

    assert!(store.is_trusted(&repo, &commands).is_err());
    assert!(store.trust(&repo, &commands).is_err());
    assert_eq!(fs::read(store.path()).unwrap(), b"not valid json");
}

#[cfg(unix)]
#[test]
fn trust_state_symlink_is_rejected_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let repo = create_repo(root.path());
    let store = TrustStore::open(root.path().join("state")).unwrap();
    let target = root.path().join("target.json");
    let original = b"[]";
    fs::write(&target, original).unwrap();
    symlink(&target, store.path()).unwrap();
    let commands = vec![repository_command("cargo test")];

    assert!(store.is_trusted(&repo, &commands).is_err());
    assert!(store.trust(&repo, &commands).is_err());
    assert_eq!(fs::read(&target).unwrap(), original);
    assert!(fs::symlink_metadata(store.path())
        .unwrap()
        .file_type()
        .is_symlink());
}
