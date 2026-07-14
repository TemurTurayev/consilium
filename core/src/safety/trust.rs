use super::{digest_commands, ensure_owner_only_dir, write_owner_only_json, VerificationCommand};
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustKey {
    pub canonical_repo: String,
    pub command_digest: String,
}

#[derive(Debug, Clone)]
pub struct TrustStore {
    path: PathBuf,
}

impl TrustStore {
    pub fn open(base: PathBuf) -> anyhow::Result<Self> {
        ensure_owner_only_dir(&base)?;
        Ok(Self {
            path: base.join("trusted-commands.json"),
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
        write_owner_only_json(&self.path, &keys)
    }

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
