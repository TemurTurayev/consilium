use consilium::safety::{
    digest_commands, write_owner_only_json, CommandSource, TrustStore, VerificationCommand,
};
#[cfg(unix)]
use serde::Serialize;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Arc;
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

#[cfg(unix)]
#[test]
fn open_store_remains_bound_to_original_directory_after_parent_substitution() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let repo = create_repo(root.path());
    let state = root.path().join("state");
    let original_state = root.path().join("original-state");
    let attacker_state = root.path().join("attacker-state");
    let store = TrustStore::open(state.clone()).unwrap();

    fs::rename(&state, &original_state).unwrap();
    fs::create_dir(&attacker_state).unwrap();
    symlink(&attacker_state, &state).unwrap();

    let commands = vec![repository_command("cargo test")];
    store.trust(&repo, &commands).unwrap();

    assert!(store.is_trusted(&repo, &commands).unwrap());
    assert!(original_state.join("trusted-commands.json").is_file());
    assert!(!attacker_state.join("trusted-commands.json").exists());
    assert_eq!(fs::read_dir(&attacker_state).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn permissive_existing_trust_file_is_tightened_before_reading() {
    let root = tempdir().unwrap();
    let repo = create_repo(root.path());
    let state = root.path().join("state");
    let store = TrustStore::open(state.clone()).unwrap();
    let commands = vec![repository_command("cargo test")];
    store.trust(&repo, &commands).unwrap();
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(
        fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
        0o666
    );

    let reopened = TrustStore::open(state).unwrap();
    assert!(reopened.is_trusted(&repo, &commands).unwrap());
    assert_eq!(
        fs::metadata(reopened.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn state_directory_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let target = root.path().join("target-state");
    let state = root.path().join("state");
    fs::create_dir(&target).unwrap();
    symlink(&target, &state).unwrap();

    assert!(TrustStore::open(state).is_err());
}

#[cfg(unix)]
#[test]
fn destination_inspection_error_prevents_serialization() {
    #[derive(Clone)]
    struct SerializationProbe(Arc<AtomicBool>);

    impl Serialize for SerializationProbe {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            self.0.store(true, Ordering::SeqCst);
            serializer.serialize_unit()
        }
    }

    let root = tempdir().unwrap();
    let serialized = Arc::new(AtomicBool::new(false));
    let invalid_name = std::ffi::OsStr::from_bytes(b"invalid\0name");
    let invalid_path = root.path().join(invalid_name);

    assert!(write_owner_only_json(&invalid_path, &SerializationProbe(serialized.clone())).is_err());
    assert!(!serialized.load(Ordering::SeqCst));
}
