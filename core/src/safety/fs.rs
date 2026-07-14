use anyhow::{bail, Context};
use serde::Serialize;
#[cfg(unix)]
use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::Write;
use std::path::Path;

#[cfg(unix)]
const OWNER_ONLY_DIR_MODE: rustix::fs::Mode = rustix::fs::Mode::from_raw_mode(0o700);
#[cfg(unix)]
const OWNER_ONLY_FILE_MODE: rustix::fs::Mode = rustix::fs::Mode::from_raw_mode(0o600);

#[cfg(unix)]
pub(crate) fn open_owner_only_dir(path: &Path) -> anyhow::Result<File> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create state directory {}", path.display()))?;

    let directory = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .with_context(|| format!("failed to securely open state directory {}", path.display()))?;
    let directory = File::from(directory);
    rustix::fs::fchmod(&directory, OWNER_ONLY_DIR_MODE).with_context(|| {
        format!(
            "failed to set owner-only permissions on state directory {}",
            path.display()
        )
    })?;
    Ok(directory)
}

#[cfg(unix)]
pub fn ensure_owner_only_dir(path: &Path) -> anyhow::Result<()> {
    open_owner_only_dir(path).map(drop)
}

#[cfg(not(unix))]
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
    Ok(())
}

#[cfg(unix)]
pub fn write_owner_only_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("state file has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .with_context(|| format!("state file has no name: {}", path.display()))?;
    let directory = open_owner_only_dir(parent)?;
    write_owner_only_json_at(&directory, file_name, value)
}

#[cfg(unix)]
pub(crate) fn write_owner_only_json_at<T: Serialize + ?Sized>(
    directory: &File,
    file_name: &OsStr,
    value: &T,
) -> anyhow::Result<()> {
    inspect_destination(directory, file_name)?;

    let (temporary_name, mut temporary_file) = create_temporary_file(directory)?;
    let mut cleanup = TemporaryEntry {
        directory,
        name: temporary_name,
        armed: true,
    };

    rustix::fs::fchmod(&temporary_file, OWNER_ONLY_FILE_MODE)
        .context("failed to set owner-only permissions on temporary state file")?;
    serde_json::to_writer(&mut temporary_file, value).context("failed to serialize state")?;
    temporary_file
        .flush()
        .context("failed to flush temporary state file")?;
    rustix::fs::fsync(&temporary_file).context("failed to sync temporary state file")?;
    rustix::fs::renameat(directory, cleanup.name.as_str(), directory, file_name)
        .context("failed to atomically replace state file")?;
    cleanup.armed = false;
    rustix::fs::fsync(directory).context("failed to sync state directory after replacement")?;
    Ok(())
}

#[cfg(unix)]
fn inspect_destination(directory: &File, file_name: &OsStr) -> anyhow::Result<()> {
    match rustix::fs::statat(directory, file_name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => match rustix::fs::FileType::from_raw_mode(stat.st_mode) {
            rustix::fs::FileType::RegularFile => Ok(()),
            rustix::fs::FileType::Symlink => bail!("refusing to replace symlinked state file"),
            _ => bail!("state path is not a regular file"),
        },
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(error).context("failed to inspect state file"),
    }
}

#[cfg(unix)]
fn create_temporary_file(directory: &File) -> anyhow::Result<(String, File)> {
    for _ in 0..128 {
        let name = format!(".trusted-commands.tmp-{:016x}", rand::random::<u64>());
        match rustix::fs::openat(
            directory,
            name.as_str(),
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            OWNER_ONLY_FILE_MODE,
        ) {
            Ok(file) => return Ok((name, File::from(file))),
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(error).context("failed to create temporary state file"),
        }
    }
    bail!("failed to allocate a unique temporary state file")
}

#[cfg(unix)]
struct TemporaryEntry<'a> {
    directory: &'a File,
    name: String,
    armed: bool,
}

#[cfg(unix)]
impl Drop for TemporaryEntry<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = rustix::fs::unlinkat(
                self.directory,
                self.name.as_str(),
                rustix::fs::AtFlags::empty(),
            );
        }
    }
}

#[cfg(not(unix))]
pub fn write_owner_only_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("state file has no parent: {}", path.display()))?;
    ensure_owner_only_dir(parent)?;

    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                bail!(
                    "refusing to replace symlinked state file {}",
                    path.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect state file {}", path.display()));
        }
    }

    let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to create temporary state file beside {}",
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
