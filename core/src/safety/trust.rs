#[cfg(not(unix))]
use super::fs::{ensure_owner_only_dir, write_owner_only_json};
#[cfg(unix)]
use super::fs::{open_owner_only_dir, write_owner_only_json_at};
use super::{digest_commands, VerificationCommand};
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
#[cfg(not(unix))]
use std::fs;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;

const TRUST_FILE_NAME: &str = "trusted-commands.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustKey {
    pub canonical_repo: String,
    pub command_digest: String,
}

#[derive(Debug, Clone)]
pub struct TrustStore {
    path: PathBuf,
    #[cfg(unix)]
    directory: Arc<File>,
}

impl TrustStore {
    pub fn open(base: PathBuf) -> anyhow::Result<Self> {
        #[cfg(unix)]
        let directory = Arc::new(open_owner_only_dir(&base)?);
        #[cfg(not(unix))]
        ensure_owner_only_dir(&base)?;

        Ok(Self {
            path: base.join(TRUST_FILE_NAME),
            #[cfg(unix)]
            directory,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_trusted(
        &self,
        repo: &Path,
        commands: &[VerificationCommand],
    ) -> anyhow::Result<bool> {
        let key = trust_key(repo, commands)?;
        Ok(self.load()?.contains(&key))
    }

    pub fn trust(&self, repo: &Path, commands: &[VerificationCommand]) -> anyhow::Result<()> {
        let key = trust_key(repo, commands)?;
        let mut keys = self.load()?;
        keys.retain(|existing| existing.canonical_repo != key.canonical_repo);
        keys.push(key);

        #[cfg(unix)]
        return write_owner_only_json_at(&self.directory, TRUST_FILE_NAME.as_ref(), &keys);
        #[cfg(not(unix))]
        write_owner_only_json(&self.path, &keys)
    }

    #[cfg(unix)]
    fn load(&self) -> anyhow::Result<Vec<TrustKey>> {
        let file = match rustix::fs::openat(
            &self.directory,
            TRUST_FILE_NAME,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) {
            Ok(file) => file,
            Err(rustix::io::Errno::NOENT) => return Ok(Vec::new()),
            Err(error) => return Err(error).context("failed to securely open trust store"),
        };
        let mut file = File::from(file);
        let stat = rustix::fs::fstat(&file).context("failed to inspect opened trust store")?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
            bail!("trust store is not a regular file");
        }
        rustix::fs::fchmod(&file, rustix::fs::Mode::from_raw_mode(0o600))
            .context("failed to tighten trust store permissions")?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .context("failed to read trust store")?;
        serde_json::from_slice(&bytes).context("failed to parse trust store")
    }

    #[cfg(not(unix))]
    fn load(&self) -> anyhow::Result<Vec<TrustKey>> {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    bail!(
                        "refusing to read symlinked trust store {}",
                        self.path.display()
                    );
                }
                if !metadata.is_file() {
                    bail!("trust store is not a file: {}", self.path.display());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect trust store {}", self.path.display())
                });
            }
        }

        let bytes = fs::read(&self.path)
            .with_context(|| format!("failed to read trust store {}", self.path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse trust store {}", self.path.display()))
    }
}

fn trust_key(repo: &Path, commands: &[VerificationCommand]) -> anyhow::Result<TrustKey> {
    let canonical_repo = repo
        .canonicalize()
        .with_context(|| format!("failed to canonicalize repository {}", repo.display()))?
        .display()
        .to_string();
    Ok(TrustKey {
        canonical_repo,
        command_digest: digest_commands(commands),
    })
}
