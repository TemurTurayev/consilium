use anyhow::{bail, Context};
use serde::Serialize;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn ensure_owner_only_dir(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                bail!(
                    "refusing to use symlinked state directory {}",
                    path.display()
                );
            }
            if !metadata.is_dir() {
                bail!("state path is not a directory: {}", path.display());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .with_context(|| format!("failed to create state directory {}", path.display()))?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect state directory {}", path.display()));
        }
    }

    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "failed to set owner-only permissions on state directory {}",
            path.display()
        )
    })?;

    Ok(())
}

pub fn write_owner_only_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("state file has no parent: {}", path.display()))?;
    ensure_owner_only_dir(parent)?;

    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing to replace symlinked state file {}",
                path.display()
            );
        }
    }

    let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to create temporary state file beside {}",
            path.display()
        )
    })?;

    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .with_context(|| {
            format!(
                "failed to set owner-only permissions on temporary state file for {}",
                path.display()
            )
        })?;

    serde_json::to_writer(&mut temporary, value)
        .with_context(|| format!("failed to serialize state for {}", path.display()))?;
    temporary
        .flush()
        .with_context(|| format!("failed to flush state file for {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync state file for {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to atomically replace state file {}", path.display()))?;

    Ok(())
}
