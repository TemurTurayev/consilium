use std::ffi::OsStr;

pub struct CliStatus {
    pub binary: String,
    pub found: bool,
    pub version: Option<String>,
}

/// Checks `<binary> --version` resolving through PATH (or an override for tests).
pub fn check_with_path(binary: &str, path_override: Option<&OsStr>) -> CliStatus {
    let mut cmd = std::process::Command::new(binary);
    cmd.arg("--version");
    if let Some(path) = path_override {
        cmd.env("PATH", path);
    }
    match cmd.output() {
        Ok(out) if out.status.success() => CliStatus {
            binary: binary.to_string(),
            found: true,
            version: Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        },
        _ => CliStatus {
            binary: binary.to_string(),
            found: false,
            version: None,
        },
    }
}

pub fn check(binary: &str) -> CliStatus {
    check_with_path(binary, None)
}

pub fn run_doctor() -> Vec<CliStatus> {
    ["claude", "codex", "gemini"]
        .iter()
        .map(|b| check(b))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_bin_dir(name: &str, output: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho \"{output}\"\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    #[test]
    fn detects_installed_cli_and_version() {
        let dir = fake_bin_dir("fakecli", "fakecli 9.9.9");
        let status = check_with_path("fakecli", Some(dir.path().as_os_str()));
        assert!(status.found);
        assert_eq!(status.version.as_deref(), Some("fakecli 9.9.9"));
    }

    #[test]
    fn reports_missing_cli() {
        let dir = tempfile::tempdir().unwrap(); // empty dir on PATH
        let status = check_with_path("definitely-not-installed", Some(dir.path().as_os_str()));
        assert!(!status.found);
        assert!(status.version.is_none());
    }
}
