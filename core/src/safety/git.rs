use super::ensure_owner_only_dir;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedWorktree {
    pub id: String,
    pub source_repo: PathBuf,
    pub path: PathBuf,
    pub base_commit: String,
}

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
    let repository = require_repository(source)?;
    ensure_owner_only_dir(state_root)?;
    let worktrees_root = state_root.join("worktrees");
    ensure_owner_only_dir(&worktrees_root)?;

    let (id, path) = allocate_worktree_path(&worktrees_root)?;
    let args = vec![
        OsString::from("worktree"),
        OsString::from("add"),
        OsString::from("--detach"),
        OsString::from("--"),
        path.as_os_str().to_owned(),
        OsString::from(&repository.head),
    ];

    if let Err(error) = required_git_output(&repository.root, args, "create detached worktree") {
        return match cleanup_failed_worktree(&repository.root, &worktrees_root, &path) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(error.context(format!(
                "detached worktree cleanup also failed: {cleanup_error:#}"
            ))),
        };
    }

    Ok(PreparedWorktree {
        id,
        source_repo: repository.root,
        path,
        base_commit: repository.head,
    })
}

pub fn remove_worktree(prepared: &PreparedWorktree) -> Result<()> {
    let source_repo = prepared.source_repo.canonicalize().with_context(|| {
        format!(
            "canonicalize source repository {}",
            prepared.source_repo.display()
        )
    })?;
    let remove = run_git(
        &source_repo,
        vec![
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--force"),
            OsString::from("--"),
            prepared.path.as_os_str().to_owned(),
        ],
    )
    .context("run Git worktree removal")?;

    let prune = required_git_output(
        &source_repo,
        static_args(&["worktree", "prune"]),
        "prune Git worktrees",
    );
    if !remove.status.success() && prepared.path.exists() {
        bail!(
            "failed to remove detached worktree: {}",
            diagnostic_stderr(&remove)
        );
    }
    prune?;
    Ok(())
}

pub fn source_is_applyable(source: &Path, base: &str) -> Result<bool> {
    let state = inspect_repository(source)?;
    if state.kind != RepositoryKind::Git {
        return Ok(false);
    }
    Ok(state.head.as_deref() == Some(base) && state.clean)
}

fn require_repository(source: &Path) -> Result<GitRepository> {
    let canonical = source
        .canonicalize()
        .with_context(|| format!("canonicalize {}", source.display()))?;
    let Some(root) = repository_root(&canonical)? else {
        bail!(
            "safe worktree mode requires a Git repository; initialize Git and create a commit first"
        );
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

fn allocate_worktree_path(root: &Path) -> Result<(String, PathBuf)> {
    for _ in 0..128 {
        let id = format!(
            "{:016x}{:016x}",
            rand::random::<u64>(),
            rand::random::<u64>()
        );
        let path = root.join(&id);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((id, path)),
            Ok(_) => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect worktree path {}", path.display()));
            }
        }
    }
    bail!("failed to allocate a unique detached worktree path")
}

fn cleanup_failed_worktree(source_repo: &Path, worktrees_root: &Path, path: &Path) -> Result<()> {
    let _ = run_git(
        source_repo,
        vec![
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--force"),
            OsString::from("--"),
            path.as_os_str().to_owned(),
        ],
    );
    if path.parent() != Some(worktrees_root) {
        bail!("refusing to clean a path outside the generated worktree directory");
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(path)
                .with_context(|| format!("remove partial worktree symlink {}", path.display()))?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path)
                .with_context(|| format!("remove partial worktree directory {}", path.display()))?;
        }
        Ok(_) => {
            fs::remove_file(path)
                .with_context(|| format!("remove partial worktree file {}", path.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect partial worktree path {}", path.display()));
        }
    }
    required_git_output(
        source_repo,
        static_args(&["worktree", "prune", "--expire", "now"]),
        "prune partial Git worktree registration",
    )?;
    Ok(())
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
    Command::new("git").args(&args).current_dir(cwd).output()
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
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn ascii_text(bytes: &[u8]) -> Result<String> {
    let bytes = trim_line_end(bytes);
    if bytes.is_empty() || !bytes.is_ascii() {
        bail!("Git returned a non-ASCII commit identifier")
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

fn trim_line_end(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(unix)]
fn path_from_output(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let bytes = trim_line_end(bytes);
    (!bytes.is_empty()).then(|| PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn path_from_output(bytes: &[u8]) -> Option<PathBuf> {
    let bytes = trim_line_end(bytes);
    std::str::from_utf8(bytes)
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
