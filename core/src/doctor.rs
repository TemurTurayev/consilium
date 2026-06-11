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
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let version = if stdout.is_empty() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if stderr.is_empty() {
                    None
                } else {
                    Some(stderr)
                }
            } else {
                Some(stdout)
            };
            CliStatus {
                binary: binary.to_string(),
                found: true,
                version,
            }
        }
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

    fn fake_bin_dir_script(name: &str, script: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    fn fake_bin_dir(name: &str, output: &str) -> tempfile::TempDir {
        fake_bin_dir_script(name, &format!("#!/bin/sh\necho \"{output}\"\n"))
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

    #[test]
    fn version_falls_back_to_stderr() {
        let dir = fake_bin_dir_script("stderrcli", "#!/bin/sh\necho \"ver 1.0\" >&2\n");
        let status = check_with_path("stderrcli", Some(dir.path().as_os_str()));
        assert!(status.found);
        assert_eq!(status.version.as_deref(), Some("ver 1.0"));
    }

    #[test]
    fn silent_binary_yields_no_version() {
        let dir = fake_bin_dir_script("silentcli", "#!/bin/sh\nexit 0\n");
        let status = check_with_path("silentcli", Some(dir.path().as_os_str()));
        assert!(status.found);
        assert!(status.version.is_none());
    }
}
