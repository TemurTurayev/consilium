//! Saved credentials for the provider CLIs Consilium launches.
//!
//! `~/.consilium/secrets.env` holds what a provider's headless CLI needs to
//! sign in — today Claude's `CLAUDE_CODE_OAUTH_TOKEN` (from `claude
//! setup-token`). A terminal gets it by sourcing that file from the shell
//! profile. A process started by a desktop app never runs the shell profile:
//! the MCP server inside the Claude app has no token, so every Claude role
//! failed there with a 401 while the same command worked in a terminal.
//! Consilium therefore reads the file itself and hands each provider CLI the
//! credentials its environment is missing.

use std::path::{Path, PathBuf};

/// `~/.consilium/secrets.env`, or `None` when `$HOME` is not set.
pub fn default_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
    Some(PathBuf::from(home).join(".consilium").join("secrets.env"))
}

/// The `KEY=VALUE` pairs of a shell-style env file, in file order. Understands
/// what such a file holds in practice — an optional `export`, `#` comments and
/// one pair of quotes around the value — and skips every other line; it does
/// not expand variables or run anything.
pub fn parse_env_file(raw: &str) -> Vec<(String, String)> {
    raw.lines().filter_map(parse_assignment).collect()
}

fn parse_assignment(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let assignment = line
        .strip_prefix("export")
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .map_or(line, str::trim_start);
    let (key, value) = assignment.split_once('=')?;
    let key = key.trim();
    is_env_name(key).then(|| (key.to_string(), unquote(value.trim()).to_string()))
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strips one pair of matching quotes; an unmatched quote stays in the value.
fn unquote(value: &str) -> &str {
    ['"', '\'']
        .into_iter()
        .find_map(|quote| {
            value
                .strip_prefix(quote)
                .and_then(|rest| rest.strip_suffix(quote))
        })
        .unwrap_or(value)
}

/// The credentials in `file` for those of `keys` that `is_set` reports
/// missing from the environment. The file is read only when something is
/// missing; an unreadable or absent file yields nothing. Where the file
/// assigns a name twice the last value wins, as it would in a shell.
pub fn missing_credentials(
    keys: &[&str],
    file: &Path,
    is_set: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let missing: Vec<&str> = keys.iter().copied().filter(|key| !is_set(key)).collect();
    if missing.is_empty() {
        return Vec::new();
    }
    let Ok(raw) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let saved = parse_env_file(&raw);
    missing
        .into_iter()
        .filter_map(|key| {
            saved
                .iter()
                .rev()
                .find(|(name, value)| name == key && !value.is_empty())
                .cloned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(raw: &str) -> Vec<(String, String)> {
        parse_env_file(raw)
    }

    fn pair(key: &str, value: &str) -> (String, String) {
        (key.to_string(), value.to_string())
    }

    fn secrets_file(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.env");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn parses_an_exported_assignment() {
        assert_eq!(
            pairs("export CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-abc\n"),
            [pair("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-abc")]
        );
    }

    #[test]
    fn parses_a_bare_assignment() {
        assert_eq!(pairs("TOKEN=abc"), [pair("TOKEN", "abc")]);
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let raw = "# Consilium credentials — LOCAL ONLY\n\n   # indented comment\nA=1\n";
        assert_eq!(pairs(raw), [pair("A", "1")]);
    }

    #[test]
    fn strips_one_pair_of_matching_quotes() {
        assert_eq!(pairs("A=\"two words\""), [pair("A", "two words")]);
        assert_eq!(pairs("A='single'"), [pair("A", "single")]);
        // an unmatched quote is part of the value
        assert_eq!(pairs("A=\"open"), [pair("A", "\"open")]);
    }

    #[test]
    fn trims_whitespace_around_the_key_and_value() {
        assert_eq!(pairs("  export   A=1  \r\n"), [pair("A", "1")]);
    }

    #[test]
    fn keeps_an_equals_sign_inside_the_value() {
        assert_eq!(pairs("A=b=c"), [pair("A", "b=c")]);
    }

    #[test]
    fn skips_lines_that_are_not_assignments() {
        let raw = "source ~/.other\nnot an assignment\n1BAD=x\nBAD-NAME=x\n=novalue\nOK_1=y";
        assert_eq!(pairs(raw), [pair("OK_1", "y")]);
    }

    #[test]
    fn returns_only_the_requested_keys_that_are_missing() {
        let (_dir, path) = secrets_file("export WANTED=w\nexport ALREADY=a\nexport OTHER=o\n");
        let found = missing_credentials(&["WANTED", "ALREADY"], &path, |key| key == "ALREADY");
        assert_eq!(found, [pair("WANTED", "w")]);
    }

    /// A shell that sources the file sees the last assignment of a name.
    #[test]
    fn the_last_assignment_of_a_key_wins() {
        let (_dir, path) = secrets_file("A=old\nA=new\n");
        assert_eq!(
            missing_credentials(&["A"], &path, |_| false),
            [pair("A", "new")]
        );
    }

    #[test]
    fn an_empty_value_is_not_a_credential() {
        let (_dir, path) = secrets_file("A=\nB=\"\"\n");
        assert!(missing_credentials(&["A", "B"], &path, |_| false).is_empty());
    }

    #[test]
    fn a_missing_file_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.env");
        assert!(missing_credentials(&["A"], &path, |_| false).is_empty());
    }

    #[test]
    fn default_path_is_under_the_consilium_home() {
        let path = default_path().expect("HOME is set in the test environment");
        assert!(path.ends_with(".consilium/secrets.env"), "{path:?}");
    }
}
