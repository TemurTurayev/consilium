#[cfg(unix)]
use super::fs::write_owner_only_json_at;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
#[cfg(any(not(unix), all(test, unix)))]
use std::fs;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub enum RepositoryKind {
    Git,
    NonGit,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct RepositoryState {
    pub canonical_path: String,
    pub git_root: Option<String>,
    pub kind: RepositoryKind,
    pub head: Option<String>,
    pub clean: bool,
    pub tracked_dirty: Vec<String>,
    pub untracked: Vec<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRepository {
    pub root: PathBuf,
    pub head: String,
}

/// A live cleanup capability for a worktree created by this process.
///
/// Only the four documented fields are serialized. Removal additionally needs
/// the private descriptor-bound capability, so arbitrary deserialized data can
/// never authorize `--force` deletion of another worktree.
#[derive(Debug, Clone, Serialize)]
pub struct PreparedWorktree {
    pub id: String,
    pub source_repo: PathBuf,
    pub path: PathBuf,
    pub base_commit: String,
    #[serde(skip)]
    capability: CleanupCapability,
}

/// Persistable display data. It is not cleanup authority by itself; callers
/// must pass it to `reopen_prepared_worktree` with the trusted app state root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedWorktreeSummary {
    pub id: String,
    pub source_repo: PathBuf,
    pub path: PathBuf,
    pub base_commit: String,
}

impl From<&PreparedWorktree> for PreparedWorktreeSummary {
    fn from(prepared: &PreparedWorktree) -> Self {
        Self {
            id: prepared.id.clone(),
            source_repo: prepared.source_repo.clone(),
            path: prepared.path.clone(),
            base_commit: prepared.base_commit.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct CleanupCapability {
    id: String,
    source_repo: PathBuf,
    path: PathBuf,
    base_commit: String,
    marker_name: String,
    token: String,
    removed: Arc<AtomicBool>,
    #[cfg(unix)]
    state_dir: Arc<File>,
    #[cfg(unix)]
    worktrees_dir: Arc<File>,
    #[cfg(unix)]
    entry_dir: Arc<File>,
    #[cfg(unix)]
    state_identity: FileIdentity,
    #[cfg(unix)]
    worktrees_identity: FileIdentity,
    #[cfg(unix)]
    entry_identity: FileIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WorktreeMarker {
    id: String,
    source_repo: PathBuf,
    path: PathBuf,
    base_commit: String,
    token: String,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy)]
enum CreatePhase {
    BeforeGit,
    AfterGit,
}

#[derive(Debug)]
#[allow(dead_code)]
struct CreateHookContext<'a> {
    state_root: &'a Path,
    worktrees_root: &'a Path,
    entry_path: &'a Path,
}

type CreateHook<'a> = dyn Fn(CreatePhase, &CreateHookContext<'_>) -> Result<()> + 'a;

pub fn inspect_repository(path: &Path) -> Result<RepositoryState> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", path.display()))?;
    let canonical_path = canonical.display().to_string();
    let Some(canonical_root) = repository_root(&canonical)? else {
        return Ok(non_git_repository(canonical_path));
    };

    let mut tracked_dirty = nul_paths(&required_git_output(
        &canonical_root,
        static_args(&["diff", "--name-only", "-z"]),
        "inspect tracked changes",
    )?);
    tracked_dirty.extend(nul_paths(&required_git_output(
        &canonical_root,
        static_args(&["diff", "--cached", "--name-only", "-z"]),
        "inspect staged changes",
    )?));
    let tracked_dirty = tracked_dirty.into_iter().collect::<Vec<_>>();
    let untracked = nul_paths(&required_git_output(
        &canonical_root,
        static_args(&["ls-files", "--others", "--exclude-standard", "-z"]),
        "inspect untracked files",
    )?)
    .into_iter()
    .collect::<Vec<_>>();

    Ok(RepositoryState {
        canonical_path,
        git_root: Some(canonical_root.display().to_string()),
        kind: RepositoryKind::Git,
        head: optional_git_ascii(&canonical_root, &["rev-parse", "--verify", "HEAD^{commit}"]),
        clean: tracked_dirty.is_empty() && untracked.is_empty(),
        tracked_dirty,
        untracked,
        branch: optional_git_lossy(
            &canonical_root,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        ),
    })
}

pub fn create_detached_worktree(source: &Path, state_root: &Path) -> Result<PreparedWorktree> {
    #[cfg(not(unix))]
    {
        let _ = (source, state_root);
        bail!(
            "safe detached worktree mode is unsupported on this platform; use a Unix host or WSL"
        );
    }
    #[cfg(unix)]
    create_detached_worktree_inner(source, state_root, None)
}

#[cfg(unix)]
fn create_detached_worktree_inner(
    source: &Path,
    state_root: &Path,
    hook: Option<&CreateHook<'_>>,
) -> Result<PreparedWorktree> {
    let repository = require_repository(source)?;
    let state_root = prospective_canonical_path(state_root)?;
    if state_root == repository.root || state_root.starts_with(&repository.root) {
        bail!("Consilium state root must be outside the source repository");
    }

    let capability = prepare_unix_capability(&repository, &state_root)?;
    let mut reservation = CreationReservation::new(capability);

    let context = CreateHookContext {
        state_root: &state_root,
        worktrees_root: reservation
            .capability()
            .path
            .parent()
            .context("worktree path has no parent")?,
        entry_path: &reservation.capability().path,
    };
    if let Some(hook) = hook {
        hook(CreatePhase::BeforeGit, &context)?;
    }
    validate_live_paths(reservation.capability())
        .context("state path identity changed before Git worktree add")?;

    let args = vec![
        OsString::from("worktree"),
        OsString::from("add"),
        OsString::from("--no-checkout"),
        OsString::from("--detach"),
        OsString::from("--"),
        reservation.capability().path.as_os_str().to_owned(),
        OsString::from(&repository.head),
    ];
    if let Err(error) = required_git_output(&repository.root, args, "create detached worktree") {
        return match reservation.cleanup() {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(error.context(format!(
                "partial worktree was preserved because cleanup could not prove its identity: {cleanup_error:#}"
            ))),
        };
    }
    if let Err(error) = materialize_without_repository_commands(
        &repository.root,
        &reservation.capability().path,
        &repository.head,
        &reservation.capability().entry_dir,
    ) {
        return match reservation.cleanup() {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(error.context(format!(
                "materialization failed and cleanup could not prove identity: {cleanup_error:#}"
            ))),
        };
    }

    if let Some(hook) = hook {
        hook(CreatePhase::AfterGit, &context)?;
    }
    validate_live_paths(reservation.capability()).context(
        "state path identity changed during Git worktree add; preserving all paths for recovery",
    )?;
    if let Err(error) = write_marker(reservation.capability()) {
        return match reservation.cleanup() {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(error.context(format!(
                "worktree marker failed and cleanup could not prove identity: {cleanup_error:#}"
            ))),
        };
    }

    let capability = reservation.disarm();
    Ok(PreparedWorktree {
        id: capability.id.clone(),
        source_repo: capability.source_repo.clone(),
        path: capability.path.clone(),
        base_commit: capability.base_commit.clone(),
        capability,
    })
}

#[cfg(unix)]
struct CreationReservation {
    capability: Option<CleanupCapability>,
}

#[cfg(unix)]
impl CreationReservation {
    fn new(capability: CleanupCapability) -> Self {
        Self {
            capability: Some(capability),
        }
    }

    fn capability(&self) -> &CleanupCapability {
        self.capability.as_ref().expect("armed reservation")
    }

    fn cleanup(&mut self) -> Result<()> {
        let result = secure_cleanup(self.capability(), false);
        if result.is_ok() {
            self.capability.take();
        }
        result
    }

    fn disarm(mut self) -> CleanupCapability {
        self.capability.take().expect("armed reservation")
    }
}

#[cfg(unix)]
impl Drop for CreationReservation {
    fn drop(&mut self) {
        if let Some(capability) = &self.capability {
            let _ = secure_cleanup(capability, false);
        }
    }
}

pub fn remove_worktree(prepared: &PreparedWorktree) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = prepared;
        bail!("safe detached worktree cleanup is unsupported on this platform; use a Unix host or WSL");
    }
    #[cfg(unix)]
    {
        validate_public_handle(prepared)?;
        if prepared.capability.removed.load(Ordering::Acquire) {
            return Ok(());
        }
        validate_marker(&prepared.capability)?;
        let registration =
            worktree_registration(&prepared.capability.source_repo, &prepared.capability.path)?;
        if registration.locked {
            bail!("registered detached worktree is locked; unlock it before cleanup");
        }
        if !registration.present {
            if expected_entry_exists(&prepared.capability)? {
                bail!("detached worktree is no longer registered at the capability-bound path");
            }
            remove_capability_marker(&prepared.capability)?;
            prepared.capability.removed.store(true, Ordering::Release);
            return Ok(());
        }
        secure_cleanup(&prepared.capability, true)
    }
}

/// Rebuild a cleanup capability after a process restart. The state root is an
/// app-owned trust anchor; the summary alone never grants deletion authority.
#[cfg(unix)]
pub fn reopen_prepared_worktree(
    state_root: &Path,
    summary: &PreparedWorktreeSummary,
) -> Result<PreparedWorktree> {
    if !valid_id(&summary.id) {
        bail!("invalid prepared worktree id");
    }
    let state_root = prospective_canonical_path(state_root)?;
    let state_dir = Arc::new(open_existing_owner_only_path(&state_root)?);
    let state_identity = identity(&state_dir)?;
    validate_fd_path(&state_dir, &state_root, state_identity)?;
    let worktrees = open_child_directory(&state_dir, OsStr::new("worktrees"), false)?;
    require_owner_only(&worktrees, "worktrees state directory")?;
    let worktrees_dir = Arc::new(worktrees);
    let worktrees_identity = identity(&worktrees_dir)?;
    let worktrees_path = state_root.join("worktrees");
    validate_fd_path(&worktrees_dir, &worktrees_path, worktrees_identity)?;
    let expected_path = worktrees_path.join(&summary.id);
    if summary.path != expected_path {
        bail!("prepared worktree path is outside the trusted state root");
    }

    let entry = open_child_directory(&worktrees_dir, OsStr::new(&summary.id), false)?;
    require_owner_only(&entry, "prepared worktree directory")?;
    let entry_identity = identity(&entry)?;
    if entry_identity.device != worktrees_identity.device {
        bail!("prepared worktree crosses a filesystem device boundary");
    }
    let entry_dir = Arc::new(entry);
    let marker_name = format!(".run-{}.json", summary.id);
    let stored = read_marker_at(&worktrees_dir, OsStr::new(&marker_name))?;
    if stored.id != summary.id
        || stored.source_repo != summary.source_repo
        || stored.path != summary.path
        || stored.base_commit != summary.base_commit
    {
        bail!("persisted worktree marker does not match the supplied summary");
    }
    if !valid_id(&stored.token) {
        bail!("persisted worktree marker contains an invalid cleanup token");
    }
    let repository = require_repository(&summary.source_repo)?;
    if repository.root != summary.source_repo {
        bail!("prepared worktree source is not the canonical repository root");
    }
    let registration = worktree_registration(&repository.root, &summary.path)?;
    if !registration.present {
        bail!("prepared worktree is no longer registered");
    }

    let capability = CleanupCapability {
        id: summary.id.clone(),
        source_repo: summary.source_repo.clone(),
        path: summary.path.clone(),
        base_commit: summary.base_commit.clone(),
        marker_name,
        token: stored.token,
        removed: Arc::new(AtomicBool::new(false)),
        state_dir,
        worktrees_dir,
        entry_dir,
        state_identity,
        worktrees_identity,
        entry_identity,
    };
    validate_live_paths(&capability)?;
    validate_marker(&capability)?;
    Ok(PreparedWorktree {
        id: summary.id.clone(),
        source_repo: summary.source_repo.clone(),
        path: summary.path.clone(),
        base_commit: summary.base_commit.clone(),
        capability,
    })
}

#[cfg(not(unix))]
pub fn reopen_prepared_worktree(
    _state_root: &Path,
    _summary: &PreparedWorktreeSummary,
) -> Result<PreparedWorktree> {
    bail!("safe detached worktree recovery is unsupported on this platform; use a Unix host or WSL")
}

#[cfg(unix)]
fn expected_entry_exists(capability: &CleanupCapability) -> Result<bool> {
    match rustix::fs::statat(
        &capability.worktrees_dir,
        capability.id.as_str(),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(error).context("inspect capability-bound worktree entry"),
    }
}

#[cfg(not(unix))]
fn expected_entry_exists(capability: &CleanupCapability) -> Result<bool> {
    Ok(capability.path.exists())
}

#[cfg(unix)]
fn remove_capability_marker(capability: &CleanupCapability) -> Result<()> {
    remove_marker_at(capability)
}

#[cfg(not(unix))]
fn remove_capability_marker(capability: &CleanupCapability) -> Result<()> {
    let marker = capability
        .path
        .parent()
        .unwrap()
        .join(&capability.marker_name);
    match fs::remove_file(marker) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("remove worktree cleanup marker"),
    }
}

pub fn source_is_applyable(source: &Path, base: &str) -> Result<bool> {
    let state = inspect_repository(source)?;
    if state.kind != RepositoryKind::Git {
        return Ok(false);
    }
    Ok(state.head.as_deref() == Some(base) && state.clean)
}

fn validate_public_handle(prepared: &PreparedWorktree) -> Result<()> {
    let capability = &prepared.capability;
    if prepared.id != capability.id
        || prepared.source_repo != capability.source_repo
        || prepared.path != capability.path
        || prepared.base_commit != capability.base_commit
        || !valid_id(&prepared.id)
        || prepared.path.parent().and_then(Path::file_name) != Some(OsStr::new("worktrees"))
        || prepared.path.file_name() != Some(OsStr::new(&prepared.id))
    {
        bail!("prepared worktree fields do not match the live cleanup capability");
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Default)]
struct WorktreeRegistration {
    present: bool,
    locked: bool,
}

fn worktree_registration(source_repo: &Path, expected: &Path) -> Result<WorktreeRegistration> {
    let output = required_git_output(
        source_repo,
        static_args(&["worktree", "list", "--porcelain", "-z"]),
        "inspect registered Git worktrees",
    )?;
    let mut result = WorktreeRegistration::default();
    let mut selected = false;
    for field in output.stdout.split(|byte| *byte == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            selected = path_bytes_equal(path, expected);
            result.present |= selected;
        } else if selected && (field == b"locked" || field.starts_with(b"locked ")) {
            result.locked = true;
        }
    }
    Ok(result)
}

#[cfg(unix)]
fn path_bytes_equal(bytes: &[u8], path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    bytes == path.as_os_str().as_bytes()
}

#[cfg(not(unix))]
fn path_bytes_equal(bytes: &[u8], path: &Path) -> bool {
    std::str::from_utf8(bytes).ok() == path.to_str()
}

fn prospective_canonical_path(path: &Path) -> Result<PathBuf> {
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        bail!("state root must not contain '..'");
    }
    if matches!(std::fs::symlink_metadata(path), Ok(metadata) if metadata.file_type().is_symlink())
    {
        bail!(
            "refusing to use symlinked state directory {}",
            path.display()
        );
    }
    let absolute = absolute_state_path(path)?;
    let mut cursor = absolute;
    let mut suffix = Vec::new();
    loop {
        match cursor.canonicalize() {
            Ok(mut ancestor) => {
                for component in suffix.iter().rev() {
                    ancestor.push(component);
                }
                return Ok(ancestor);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor
                    .file_name()
                    .context("state root has no existing ancestor")?
                    .to_os_string();
                suffix.push(name);
                if !cursor.pop() {
                    bail!("state root has no existing ancestor");
                }
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("resolve state root {}", cursor.display()));
            }
        }
    }
}

#[cfg(unix)]
fn absolute_state_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => normalized.push(name),
            Component::ParentDir => bail!("state root must not contain '..'"),
            Component::Prefix(_) => bail!("unsupported state path prefix"),
        }
    }
    Ok(normalized)
}

#[cfg(not(unix))]
fn absolute_state_path(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

#[cfg(unix)]
fn open_filesystem_root() -> Result<File> {
    Ok(File::from(rustix::fs::open(
        "/",
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?))
}

#[cfg(unix)]
fn open_child_directory(parent: &File, name: &OsStr, create: bool) -> Result<File> {
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    match rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty()) {
        Ok(file) => Ok(File::from(file)),
        Err(rustix::io::Errno::NOENT) if create => {
            match rustix::fs::mkdirat(parent, name, rustix::fs::Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error).context("create state path component"),
            }
            Ok(File::from(
                rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty())
                    .context("open newly created state path component")?,
            ))
        }
        Err(error) => Err(error).context("open state path component without following symlinks"),
    }
}

#[cfg(unix)]
fn require_owner_only(directory: &File, label: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = directory.metadata()?;
    if metadata.mode() & 0o077 != 0 {
        bail!("{label} must already be owner-only (mode 0700 or stricter)");
    }
    Ok(())
}

#[cfg(unix)]
fn secure_dedicated_state_root(directory: &File) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = directory.metadata()?;
    if metadata.mode() & 0o077 == 0 {
        return Ok(());
    }
    if metadata.uid() != rustix::process::getuid().as_raw() {
        bail!("Consilium state root must be owned by the current user");
    }
    let mut entries = rustix::fs::Dir::read_from(directory)?;
    while let Some(entry) = entries.read() {
        let entry = entry?;
        if entry.file_name().to_bytes() != b"." && entry.file_name().to_bytes() != b".." {
            bail!("Consilium state root must already be owner-only before reusing non-empty state");
        }
    }
    rustix::fs::fchmod(directory, rustix::fs::Mode::from_raw_mode(0o700))
        .context("secure newly dedicated Consilium state root")?;
    Ok(())
}

#[cfg(unix)]
fn open_existing_owner_only_path(path: &Path) -> Result<File> {
    open_state_path(path, None, false)
}

#[cfg(unix)]
fn open_state_path(
    path: &Path,
    repository_identity: Option<FileIdentity>,
    create: bool,
) -> Result<File> {
    let mut current = open_filesystem_root()?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let next = open_child_directory(&current, name, create)?;
        if repository_identity == Some(identity(&next)?) {
            bail!("Consilium state root must be outside the source repository");
        }
        current = next;
    }
    if create {
        secure_dedicated_state_root(&current)?;
    } else {
        require_owner_only(&current, "Consilium state root")?;
    }
    Ok(current)
}

#[cfg(unix)]
fn prepare_unix_capability(
    repository: &GitRepository,
    state_root: &Path,
) -> Result<CleanupCapability> {
    let repository_dir = File::open(&repository.root)?;
    let state_dir = Arc::new(open_state_path(
        state_root,
        Some(identity(&repository_dir)?),
        true,
    )?);
    let state_identity = identity(&state_dir)?;
    validate_fd_path(&state_dir, state_root, state_identity)?;

    let worktrees = open_child_directory(&state_dir, OsStr::new("worktrees"), true)?;
    require_owner_only(&worktrees, "worktrees state directory")?;
    let worktrees_dir = Arc::new(worktrees);
    let worktrees_identity = identity(&worktrees_dir)?;
    let worktrees_path = state_root.join("worktrees");
    validate_fd_path(&worktrees_dir, &worktrees_path, worktrees_identity)?;

    for _ in 0..128 {
        let id = random_id();
        match rustix::fs::mkdirat(
            &worktrees_dir,
            id.as_str(),
            rustix::fs::Mode::from_raw_mode(0o700),
        ) {
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(error).context("reserve detached worktree directory"),
            Ok(()) => {}
        }
        let entry = match open_child_directory(&worktrees_dir, OsStr::new(&id), false) {
            Ok(entry) => entry,
            Err(error) => {
                let _ = rustix::fs::unlinkat(
                    &worktrees_dir,
                    id.as_str(),
                    rustix::fs::AtFlags::REMOVEDIR,
                );
                return Err(error).context("open reserved detached worktree directory");
            }
        };
        let entry_dir = Arc::new(entry);
        let entry_identity = identity(&entry_dir)?;
        return Ok(CleanupCapability {
            id: id.clone(),
            source_repo: repository.root.clone(),
            path: worktrees_path.join(&id),
            base_commit: repository.head.clone(),
            marker_name: format!(".run-{id}.json"),
            token: random_id(),
            removed: Arc::new(AtomicBool::new(false)),
            state_dir,
            worktrees_dir,
            entry_dir,
            state_identity,
            worktrees_identity,
            entry_identity,
        });
    }
    bail!("failed to allocate a unique detached worktree path")
}

fn random_id() -> String {
    format!(
        "{:016x}{:016x}",
        rand::random::<u64>(),
        rand::random::<u64>()
    )
}

#[cfg(unix)]
fn materialize_without_repository_commands(
    source_repo: &Path,
    worktree: &Path,
    commit: &str,
    entry_dir: &File,
) -> Result<()> {
    required_git_output(
        worktree,
        static_args(&["read-tree", "HEAD"]),
        "prepare detached worktree index",
    )?;
    let listing = required_git_output(
        source_repo,
        vec![
            OsString::from("ls-tree"),
            OsString::from("-rz"),
            OsString::from("--full-tree"),
            OsString::from("-r"),
            OsString::from(commit),
            OsString::from("--"),
        ],
        "list committed worktree contents",
    )?;
    for record in listing.stdout.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("Git returned malformed tree data")?;
        let (header, path_with_separator) = record.split_at(separator);
        let path = &path_with_separator[1..];
        let mut fields = header.split(|byte| *byte == b' ');
        let mode = fields.next().context("tree entry has no mode")?;
        let kind = fields.next().context("tree entry has no type")?;
        let oid = fields.next().context("tree entry has no object id")?;
        if fields.next().is_some() || path.is_empty() {
            bail!("Git returned malformed tree entry");
        }
        materialize_tree_entry(source_repo, entry_dir, mode, kind, oid, path)?;
    }
    Ok(())
}

#[cfg(unix)]
fn materialize_tree_entry(
    source_repo: &Path,
    root: &File,
    mode: &[u8],
    kind: &[u8],
    oid: &[u8],
    path: &[u8],
) -> Result<()> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let parts = path.split(|byte| *byte == b'/').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == b"." || *part == b".." || part.contains(&0))
    {
        bail!("Git tree contains an unsafe path");
    }
    let (leaf, parents) = parts.split_last().context("tree entry has no path")?;
    let mut current = root.try_clone()?;
    for part in parents {
        let name = OsStr::from_bytes(part);
        current = match open_child_directory(&current, name, false) {
            Ok(directory) => directory,
            Err(_) => {
                match rustix::fs::mkdirat(&current, name, rustix::fs::Mode::from_raw_mode(0o700)) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error).context("create committed tree directory"),
                }
                open_child_directory(&current, name, false)?
            }
        };
    }
    let leaf = OsStr::from_bytes(leaf);
    if mode == b"160000" && kind == b"commit" {
        rustix::fs::mkdirat(&current, leaf, rustix::fs::Mode::from_raw_mode(0o700))
            .context("create committed gitlink directory")?;
        return Ok(());
    }
    if kind != b"blob" {
        bail!("unsupported Git tree object type");
    }
    let oid = std::str::from_utf8(oid).context("tree object id is not ASCII")?;
    if !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("tree object id is invalid");
    }
    let blob = required_git_output(
        source_repo,
        vec![
            OsString::from("cat-file"),
            OsString::from("blob"),
            OsString::from(oid),
        ],
        "read committed blob without checkout filters",
    )?;
    if mode == b"120000" {
        let target = OsString::from_vec(blob.stdout);
        rustix::fs::symlinkat(&target, &current, leaf).context("create committed symbolic link")?;
        return Ok(());
    }
    let permissions = match mode {
        b"100644" => 0o600,
        b"100755" => 0o700,
        _ => bail!("unsupported Git blob mode"),
    };
    let mut file = File::from(
        rustix::fs::openat(
            &current,
            leaf,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(permissions),
        )
        .context("create committed worktree file")?,
    );
    use std::io::Write;
    file.write_all(&blob.stdout)?;
    Ok(())
}

#[cfg(unix)]
fn identity(file: &File) -> Result<FileIdentity> {
    let stat = rustix::fs::fstat(file)?;
    Ok(FileIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
    })
}

#[cfg(unix)]
fn validate_fd_path(file: &File, path: &Path, expected: FileIdentity) -> Result<()> {
    let opened = File::from(
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .with_context(|| format!("open {} without following symlinks", path.display()))?,
    );
    if identity(file)? != expected || identity(&opened)? != expected {
        bail!("directory identity changed for {}", path.display());
    }
    Ok(())
}

#[cfg(unix)]
fn validate_live_paths(capability: &CleanupCapability) -> Result<()> {
    let state_root = capability
        .path
        .parent()
        .and_then(Path::parent)
        .context("missing state root")?;
    let worktrees_root = capability.path.parent().context("missing worktrees root")?;
    validate_fd_path(&capability.state_dir, state_root, capability.state_identity)?;
    validate_fd_path(
        &capability.worktrees_dir,
        worktrees_root,
        capability.worktrees_identity,
    )?;
    let stat = rustix::fs::statat(
        &capability.worktrees_dir,
        capability.id.as_str(),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )?;
    let found = FileIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
    };
    if found != capability.entry_identity || identity(&capability.entry_dir)? != found {
        bail!("generated worktree directory identity changed");
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_live_paths(capability: &CleanupCapability) -> Result<()> {
    let metadata = fs::symlink_metadata(&capability.path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("generated worktree path is not a real directory");
    }
    Ok(())
}

fn marker(capability: &CleanupCapability) -> WorktreeMarker {
    WorktreeMarker {
        id: capability.id.clone(),
        source_repo: capability.source_repo.clone(),
        path: capability.path.clone(),
        base_commit: capability.base_commit.clone(),
        token: capability.token.clone(),
    }
}

#[cfg(unix)]
fn write_marker(capability: &CleanupCapability) -> Result<()> {
    write_owner_only_json_at(
        &capability.worktrees_dir,
        OsStr::new(&capability.marker_name),
        &marker(capability),
    )
}

#[cfg(not(unix))]
fn write_marker(capability: &CleanupCapability) -> Result<()> {
    let path = capability
        .path
        .parent()
        .unwrap()
        .join(&capability.marker_name);
    write_owner_only_json(&path, &marker(capability))
}

#[cfg(unix)]
fn validate_marker(capability: &CleanupCapability) -> Result<()> {
    let stored = read_marker_at(
        &capability.worktrees_dir,
        OsStr::new(&capability.marker_name),
    )?;
    if stored != marker(capability) {
        bail!("worktree cleanup marker does not match the live capability");
    }
    Ok(())
}

#[cfg(unix)]
fn read_marker_at(directory: &File, name: &OsStr) -> Result<WorktreeMarker> {
    use std::os::unix::fs::MetadataExt;
    let mut file = File::from(
        rustix::fs::openat(
            directory,
            name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .context("open worktree cleanup marker")?,
    );
    if rustix::fs::FileType::from_raw_mode(rustix::fs::fstat(&file)?.st_mode)
        != rustix::fs::FileType::RegularFile
    {
        bail!("worktree cleanup marker is not a regular file");
    }
    if file.metadata()?.mode() & 0o077 != 0 {
        bail!("worktree cleanup marker is not owner-only");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(not(unix))]
fn validate_marker(capability: &CleanupCapability) -> Result<()> {
    let path = capability
        .path
        .parent()
        .unwrap()
        .join(&capability.marker_name);
    let stored: WorktreeMarker = serde_json::from_slice(&fs::read(path)?)?;
    if stored != marker(capability) {
        bail!("worktree cleanup marker does not match the live capability");
    }
    Ok(())
}

#[cfg(unix)]
fn secure_cleanup(capability: &CleanupCapability, remove_marker: bool) -> Result<()> {
    if capability.removed.load(Ordering::Acquire) {
        return Ok(());
    }
    validate_fd_path(
        &capability.worktrees_dir,
        capability.path.parent().context("missing worktrees root")?,
        capability.worktrees_identity,
    )?;
    let registration = worktree_registration(&capability.source_repo, &capability.path)?;
    if registration.locked {
        bail!("registered detached worktree is locked; preserving it for recovery");
    }

    let current = rustix::fs::statat(
        &capability.worktrees_dir,
        capability.id.as_str(),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    );
    let current = match current {
        Ok(stat) => FileIdentity {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
        },
        Err(rustix::io::Errno::NOENT) => {
            required_git_output(
                &capability.source_repo,
                static_args(&["worktree", "prune", "--expire", "now"]),
                "prune externally removed worktree",
            )?;
            if remove_marker {
                remove_marker_at(capability)?;
            }
            capability.removed.store(true, Ordering::Release);
            return Ok(());
        }
        Err(error) => return Err(error).context("inspect generated worktree entry"),
    };
    if current != capability.entry_identity || identity(&capability.entry_dir)? != current {
        bail!("generated worktree identity changed; preserving it for recovery");
    }

    let quarantine = format!(".cleanup-{}-{}", capability.id, random_id());
    rustix::fs::renameat(
        &capability.worktrees_dir,
        capability.id.as_str(),
        &capability.worktrees_dir,
        quarantine.as_str(),
    )
    .context("quarantine generated worktree before cleanup")?;
    let quarantined = rustix::fs::statat(
        &capability.worktrees_dir,
        quarantine.as_str(),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )?;
    let quarantined = FileIdentity {
        device: quarantined.st_dev as u64,
        inode: quarantined.st_ino as u64,
    };
    if quarantined != capability.entry_identity {
        bail!("quarantined entry identity changed; preserving it for recovery");
    }

    required_git_output(
        &capability.source_repo,
        static_args(&["worktree", "prune", "--expire", "now"]),
        "prune detached worktree registration",
    )?;
    let registration = worktree_registration(&capability.source_repo, &capability.path)?;
    if registration.present {
        bail!("Git worktree registration remains after prune; preserving quarantined files for recovery");
    }
    remove_directory_contents(
        &capability.entry_dir,
        capability.entry_identity,
        capability.entry_identity.device,
    )?;
    let final_stat = rustix::fs::statat(
        &capability.worktrees_dir,
        quarantine.as_str(),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )?;
    if (FileIdentity {
        device: final_stat.st_dev as u64,
        inode: final_stat.st_ino as u64,
    }) != capability.entry_identity
    {
        bail!("cleanup quarantine was substituted; preserving it for recovery");
    }
    rustix::fs::unlinkat(
        &capability.worktrees_dir,
        quarantine.as_str(),
        rustix::fs::AtFlags::REMOVEDIR,
    )
    .context("remove empty quarantined worktree")?;
    if remove_marker {
        remove_marker_at(capability)?;
    }
    capability.removed.store(true, Ordering::Release);
    Ok(())
}

#[cfg(unix)]
fn remove_directory_contents(
    directory: &File,
    expected: FileIdentity,
    root_device: u64,
) -> Result<()> {
    remove_directory_contents_inner(directory, expected, root_device, None)
}

#[cfg(unix)]
type CleanupHook<'a> = dyn Fn(&OsStr) -> Result<()> + 'a;

#[cfg(unix)]
fn remove_directory_contents_inner(
    directory: &File,
    expected: FileIdentity,
    root_device: u64,
    hook: Option<&CleanupHook<'_>>,
) -> Result<()> {
    if identity(directory)? != expected {
        bail!("cleanup directory identity changed; preserving it for recovery");
    }
    if expected.device != root_device {
        bail!("cleanup refuses to cross a filesystem device boundary");
    }
    let mut entries = rustix::fs::Dir::read_from(directory)?;
    while let Some(entry) = entries.read() {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let stat = rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
        let entry_identity = FileIdentity {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
        };
        if entry_identity.device != root_device {
            bail!("cleanup refuses to cross a filesystem device boundary");
        }
        let quarantine = format!(".entry-cleanup-{}", random_id());
        rustix::fs::renameat(directory, name, directory, quarantine.as_str())
            .context("quarantine worktree child before recursive cleanup")?;
        if let Some(hook) = hook {
            hook(OsStr::new(&quarantine))?;
        }
        let quarantined = rustix::fs::statat(
            directory,
            quarantine.as_str(),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )?;
        let quarantined_identity = FileIdentity {
            device: quarantined.st_dev as u64,
            inode: quarantined.st_ino as u64,
        };
        if quarantined_identity != entry_identity {
            bail!("cleanup child identity changed; preserving it for recovery");
        }
        if rustix::fs::FileType::from_raw_mode(quarantined.st_mode)
            == rustix::fs::FileType::Directory
        {
            let child = File::from(rustix::fs::openat(
                directory,
                quarantine.as_str(),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )?);
            if identity(&child)? != entry_identity {
                bail!("cleanup child directory was substituted; preserving it for recovery");
            }
            remove_directory_contents_inner(&child, entry_identity, root_device, hook)?;
            let final_stat = rustix::fs::statat(
                directory,
                quarantine.as_str(),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )?;
            if (FileIdentity {
                device: final_stat.st_dev as u64,
                inode: final_stat.st_ino as u64,
            }) != entry_identity
            {
                bail!("cleanup child identity changed after recursion; preserving it for recovery");
            }
            rustix::fs::unlinkat(
                directory,
                quarantine.as_str(),
                rustix::fs::AtFlags::REMOVEDIR,
            )?;
        } else {
            let final_stat = rustix::fs::statat(
                directory,
                quarantine.as_str(),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )?;
            if (FileIdentity {
                device: final_stat.st_dev as u64,
                inode: final_stat.st_ino as u64,
            }) != entry_identity
            {
                bail!("cleanup child identity changed before unlink; preserving it for recovery");
            }
            rustix::fs::unlinkat(directory, quarantine.as_str(), rustix::fs::AtFlags::empty())?;
        }
    }
    if identity(directory)? != expected {
        bail!("cleanup directory identity changed after traversal; preserving it for recovery");
    }
    Ok(())
}

#[cfg(unix)]
fn remove_marker_at(capability: &CleanupCapability) -> Result<()> {
    match rustix::fs::unlinkat(
        &capability.worktrees_dir,
        capability.marker_name.as_str(),
        rustix::fs::AtFlags::empty(),
    ) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(error).context("remove worktree cleanup marker"),
    }
}

#[cfg(not(unix))]
fn secure_cleanup(capability: &CleanupCapability, remove_marker: bool) -> Result<()> {
    if capability.removed.load(Ordering::Acquire) {
        return Ok(());
    }
    let output = run_git(
        &capability.source_repo,
        vec![
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--force"),
            OsString::from("--"),
            capability.path.as_os_str().to_owned(),
        ],
    )?;
    if !output.status.success() && capability.path.exists() {
        bail!(
            "failed to remove detached worktree: {}",
            diagnostic_stderr(&output)
        );
    }
    required_git_output(
        &capability.source_repo,
        static_args(&["worktree", "prune"]),
        "prune Git worktrees",
    )?;
    if remove_marker {
        let _ = fs::remove_file(
            capability
                .path
                .parent()
                .unwrap()
                .join(&capability.marker_name),
        );
    }
    capability.removed.store(true, Ordering::Release);
    Ok(())
}

fn require_repository(source: &Path) -> Result<GitRepository> {
    let canonical = source
        .canonicalize()
        .with_context(|| format!("canonicalize {}", source.display()))?;
    let Some(root) = repository_root(&canonical)? else {
        bail!("safe worktree mode requires a Git repository; initialize Git and create a commit first");
    };
    let output = run_git(
        &root,
        static_args(&["rev-parse", "--verify", "HEAD^{commit}"]),
    )
    .context("resolve repository HEAD")?;
    if !output.status.success() {
        bail!("safe worktree mode requires an existing commit; create the first commit");
    }
    let head = ascii_text(&output.stdout).context("validate repository HEAD")?;
    Ok(GitRepository { root, head })
}

fn repository_root(cwd: &Path) -> Result<Option<PathBuf>> {
    let output = match run_git(cwd, static_args(&["rev-parse", "--show-toplevel"])) {
        Ok(output) => output,
        Err(_) => return Ok(None),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let root =
        path_from_output(&output.stdout).context("Git returned an invalid repository root")?;
    let canonical = root
        .canonicalize()
        .with_context(|| format!("canonicalize Git root {}", root.display()))?;
    Ok(Some(canonical))
}

fn non_git_repository(canonical_path: String) -> RepositoryState {
    RepositoryState {
        canonical_path,
        git_root: None,
        kind: RepositoryKind::NonGit,
        head: None,
        clean: true,
        tracked_dirty: Vec::new(),
        untracked: Vec::new(),
        branch: None,
    }
}

fn static_args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn run_git(cwd: &Path, args: Vec<OsString>) -> std::io::Result<Output> {
    Command::new("git")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .args(&args)
        .current_dir(cwd)
        .output()
}

fn required_git_output(cwd: &Path, args: Vec<OsString>, operation: &str) -> Result<Output> {
    let output = run_git(cwd, args).with_context(|| operation.to_owned())?;
    if output.status.success() {
        Ok(output)
    } else {
        bail!("{operation} failed: {}", diagnostic_stderr(&output))
    }
}

fn optional_git_ascii(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = run_git(cwd, static_args(args)).ok()?;
    if !output.status.success() {
        return None;
    }
    ascii_text(&output.stdout).ok()
}

fn optional_git_lossy(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = run_git(cwd, static_args(args)).ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(strip_one_record_terminator(&output.stdout)).to_string();
    (!value.is_empty()).then_some(value)
}

fn ascii_text(bytes: &[u8]) -> Result<String> {
    let bytes = strip_one_record_terminator(bytes);
    if bytes.is_empty() || !bytes.is_ascii() {
        bail!("Git returned a non-ASCII commit identifier");
    }
    Ok(std::str::from_utf8(bytes)?.to_owned())
}

fn diagnostic_stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_owned()
}

fn nul_paths(output: &Output) -> BTreeSet<String> {
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

fn strip_one_record_terminator(bytes: &[u8]) -> &[u8] {
    let Some(without_lf) = bytes.strip_suffix(b"\n") else {
        return bytes;
    };
    without_lf.strip_suffix(b"\r").unwrap_or(without_lf)
}

#[cfg(unix)]
fn path_from_output(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let bytes = strip_one_record_terminator(bytes);
    (!bytes.is_empty()).then(|| PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn path_from_output(bytes: &[u8]) -> Option<PathBuf> {
    let bytes = strip_one_record_terminator(bytes);
    std::str::from_utf8(bytes)
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn git(cwd: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_COUNT", "3")
            .env("GIT_CONFIG_KEY_0", "commit.gpgSign")
            .env("GIT_CONFIG_VALUE_0", "false")
            .env("GIT_CONFIG_KEY_1", "core.hooksPath")
            .env("GIT_CONFIG_VALUE_1", "/dev/null")
            .env("GIT_CONFIG_KEY_2", "init.templateDir")
            .env("GIT_CONFIG_VALUE_2", "")
            .output()
            .unwrap()
    }

    fn committed_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        assert!(git(repo.path(), &["init", "-q"]).status.success());
        fs::write(repo.path().join("base.txt"), "base\n").unwrap();
        assert!(git(repo.path(), &["add", "--", "base.txt"])
            .status
            .success());
        assert!(git(
            repo.path(),
            &[
                "-c",
                "user.name=Consilium Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "base",
            ],
        )
        .status
        .success());
        repo
    }

    fn clean(repo: &Path) -> bool {
        git(repo, &["status", "--porcelain=v1"]).stdout.is_empty()
    }

    #[test]
    fn state_root_substitution_is_detected_and_preserved() {
        let repo = committed_repo();
        let outer = tempfile::tempdir().unwrap();
        let state = outer.path().join("state");
        let moved = outer.path().join("original-state");
        let hook = |phase: CreatePhase, context: &CreateHookContext<'_>| {
            if matches!(phase, CreatePhase::BeforeGit) {
                fs::rename(context.state_root, &moved)?;
                fs::create_dir(context.state_root)?;
                fs::write(context.state_root.join("attacker.txt"), "preserve")?;
            }
            Ok(())
        };

        let error = create_detached_worktree_inner(repo.path(), &state, Some(&hook)).unwrap_err();

        assert!(error.to_string().contains("identity changed"));
        assert_eq!(
            fs::read_to_string(state.join("attacker.txt")).unwrap(),
            "preserve"
        );
        assert!(moved.join("worktrees").is_dir());
        assert!(clean(repo.path()));
    }

    #[test]
    fn worktrees_parent_substitution_is_detected_and_preserved() {
        let repo = committed_repo();
        let outer = tempfile::tempdir().unwrap();
        let state = outer.path().join("state");
        let moved = outer.path().join("original-worktrees");
        let hook = |phase: CreatePhase, context: &CreateHookContext<'_>| {
            if matches!(phase, CreatePhase::BeforeGit) {
                fs::rename(context.worktrees_root, &moved)?;
                fs::create_dir(context.worktrees_root)?;
                fs::write(context.worktrees_root.join("attacker.txt"), "preserve")?;
            }
            Ok(())
        };

        let error = create_detached_worktree_inner(repo.path(), &state, Some(&hook)).unwrap_err();

        assert!(error.to_string().contains("identity changed"));
        assert_eq!(
            fs::read_to_string(state.join("worktrees/attacker.txt")).unwrap(),
            "preserve"
        );
        assert!(fs::read_dir(&moved).unwrap().next().is_some());
        assert!(clean(repo.path()));
    }

    #[test]
    fn generated_entry_substitution_is_detected_without_recursive_deletion() {
        let repo = committed_repo();
        let outer = tempfile::tempdir().unwrap();
        let state = outer.path().join("state");
        let hook = |phase: CreatePhase, context: &CreateHookContext<'_>| {
            if matches!(phase, CreatePhase::BeforeGit) {
                let moved = context.worktrees_root.join("preserved-original");
                fs::rename(context.entry_path, moved)?;
                fs::create_dir(context.entry_path)?;
                fs::write(context.entry_path.join("attacker.txt"), "preserve")?;
            }
            Ok(())
        };

        let error = create_detached_worktree_inner(repo.path(), &state, Some(&hook)).unwrap_err();

        assert!(error.to_string().contains("identity changed"));
        let attacker = fs::read_dir(state.join("worktrees"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("attacker.txt"))
            .find(|path| path.is_file())
            .unwrap();
        assert_eq!(fs::read_to_string(attacker).unwrap(), "preserve");
        assert!(clean(repo.path()));
    }

    #[test]
    fn post_git_entry_substitution_fails_closed_and_preserves_both_trees() {
        let repo = committed_repo();
        let outer = tempfile::tempdir().unwrap();
        let state = outer.path().join("state");
        let hook = |phase: CreatePhase, context: &CreateHookContext<'_>| {
            if matches!(phase, CreatePhase::AfterGit) {
                let moved = context.worktrees_root.join("completed-for-recovery");
                fs::rename(context.entry_path, &moved)?;
                fs::create_dir(context.entry_path)?;
                fs::write(context.entry_path.join("attacker.txt"), "preserve")?;
                assert!(moved.join("base.txt").is_file());
            }
            Ok(())
        };

        let error = create_detached_worktree_inner(repo.path(), &state, Some(&hook)).unwrap_err();

        assert!(error
            .to_string()
            .contains("preserving all paths for recovery"));
        assert!(state
            .join("worktrees/completed-for-recovery/base.txt")
            .is_file());
        let attacker = fs::read_dir(state.join("worktrees"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path().join("attacker.txt"))
            .find(|path| path.is_file())
            .unwrap();
        assert_eq!(fs::read_to_string(attacker).unwrap(), "preserve");
        assert!(clean(repo.path()));
    }

    #[test]
    fn partial_git_add_failure_cleans_only_descriptor_bound_entry() {
        let repo = committed_repo();
        let outer = tempfile::tempdir().unwrap();
        let state = outer.path().join("state");
        let mut reserved = None;
        let hook = |phase: CreatePhase, context: &CreateHookContext<'_>| {
            if matches!(phase, CreatePhase::BeforeGit) {
                fs::write(context.entry_path.join("force-add-failure.txt"), "block")?;
            }
            Ok(())
        };

        let result = create_detached_worktree_inner(repo.path(), &state, Some(&hook));
        if let Err(error) = &result {
            reserved = Some(error.to_string());
        }

        assert!(
            result.is_err(),
            "non-empty reserved directory must make Git add fail"
        );
        assert!(reserved.unwrap().contains("create detached worktree"));
        let entries = fs::read_dir(state.join("worktrees")).unwrap().count();
        assert_eq!(entries, 0, "partial path and marker must be removed");
        let list = git(repo.path(), &["worktree", "list", "--porcelain"]);
        assert_eq!(
            String::from_utf8_lossy(&list.stdout)
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            1
        );
        assert!(clean(repo.path()));
        assert_eq!(
            fs::read_to_string(repo.path().join("base.txt")).unwrap(),
            "base\n"
        );
    }

    #[test]
    fn early_hook_error_releases_reserved_entry() {
        let repo = committed_repo();
        let outer = tempfile::tempdir().unwrap();
        let state = outer.path().join("state");
        let hook = |phase: CreatePhase, _context: &CreateHookContext<'_>| {
            if matches!(phase, CreatePhase::BeforeGit) {
                bail!("intentional early failure");
            }
            Ok(())
        };

        let error = create_detached_worktree_inner(repo.path(), &state, Some(&hook)).unwrap_err();

        assert!(error.to_string().contains("intentional early failure"));
        assert_eq!(fs::read_dir(state.join("worktrees")).unwrap().count(), 0);
        assert!(clean(repo.path()));
    }

    #[test]
    fn recursive_cleanup_revalidates_quarantined_child_identity() {
        use std::cell::Cell;

        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("child");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("original.txt"), "preserve").unwrap();
        let directory = File::open(root.path()).unwrap();
        let root_identity = identity(&directory).unwrap();
        let fired = Cell::new(false);
        let hook = |quarantine: &OsStr| {
            if !fired.replace(true) {
                let quarantined = root.path().join(quarantine);
                let preserved = root.path().join("preserved-original");
                fs::rename(&quarantined, &preserved)?;
                fs::create_dir(&quarantined)?;
                fs::write(quarantined.join("attacker.txt"), "preserve")?;
            }
            Ok(())
        };

        let error = remove_directory_contents_inner(
            &directory,
            root_identity,
            root_identity.device,
            Some(&hook),
        )
        .unwrap_err();

        assert!(error.to_string().contains("identity changed"));
        assert_eq!(
            fs::read_to_string(root.path().join("preserved-original/original.txt")).unwrap(),
            "preserve"
        );
        let attacker = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("attacker.txt"))
            .find(|path| path.is_file())
            .unwrap();
        assert_eq!(fs::read_to_string(attacker).unwrap(), "preserve");
    }
}
