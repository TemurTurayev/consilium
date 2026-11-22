//! Where a configuration comes from: the built-in defaults, then the user's
//! own file, then the project's — each layer overriding the one before it.
//!
//! - `~/.consilium/consilium.config.json` — the user's council. It follows
//!   them into every directory, so the MCP server and the CLI run the same
//!   lineup wherever they are started.
//! - `./consilium.config.json` — what one project changes. It may be a whole
//!   configuration or just the keys that differ (typically `verify`, the
//!   project's own build and test commands).
//!
//! A project file overrides the user file key by key, and inside `roles` role
//! by role; whatever neither file sets keeps its built-in default.

use crate::config::{Config, RoleConfig};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The project-level configuration file, looked up in the working directory.
pub const PROJECT_CONFIG_FILE: &str = "consilium.config.json";

/// `~/.consilium/consilium.config.json`, or `None` when `$HOME` is not set.
pub fn user_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".consilium")
            .join(PROJECT_CONFIG_FILE),
    )
}

/// The configuration files that exist for one project directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigFiles {
    pub user: Option<PathBuf>,
    pub project: Option<PathBuf>,
}

impl ConfigFiles {
    /// The user's file and `project_dir`'s file, each kept only if it exists.
    pub fn locate(project_dir: &Path) -> Self {
        Self::locate_with_user_file(project_dir, user_config_path())
    }

    /// [`locate`](Self::locate) with the user's file named explicitly.
    pub fn locate_with_user_file(project_dir: &Path, user_file: Option<PathBuf>) -> Self {
        Self {
            user: user_file.filter(|path| path.is_file()),
            project: Some(project_dir.join(PROJECT_CONFIG_FILE)).filter(|path| path.is_file()),
        }
    }

    /// The files that exist, most general first (user, then project).
    pub fn existing(&self) -> Vec<&Path> {
        [&self.user, &self.project]
            .into_iter()
            .filter_map(|file| file.as_deref())
            .collect()
    }

    /// The files this configuration was read from, for display; `None` when
    /// there are none and the built-in defaults apply.
    pub fn describe(&self) -> Option<String> {
        let names: Vec<String> = self
            .existing()
            .into_iter()
            .map(|file| file.display().to_string())
            .collect();
        (!names.is_empty()).then(|| names.join(" + "))
    }

    /// The effective configuration: defaults, then the user's file, then the
    /// project's. A file that cannot be read or is not a JSON object is an
    /// error naming it — a broken file is never silently skipped.
    pub fn load(&self) -> anyhow::Result<Config> {
        let defaults = serde_json::to_value(Config::default())?;
        let merged = self
            .existing()
            .into_iter()
            .try_fold(defaults, |lower, file| {
                read_layer(file).map(|upper| overlay(lower, upper))
            })?;
        serde_json::from_value(merged).map_err(|e| match self.describe() {
            Some(files) => anyhow::anyhow!("invalid configuration in {files}: {e}"),
            None => e.into(),
        })
    }
}

/// One configuration file as a JSON object.
fn read_layer(file: &Path) -> anyhow::Result<Value> {
    let raw = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", file.display()))?;
    parse_object(&raw).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))
}

fn parse_object(raw: &str) -> anyhow::Result<Value> {
    let value: Value = serde_json::from_str(raw)?;
    if !value.is_object() {
        anyhow::bail!("a configuration file must hold a JSON object");
    }
    Ok(value)
}

/// `upper` laid over `lower`: each of its keys replaces the same key below,
/// except `roles`, where each named role replaces that role only.
fn overlay(lower: Value, upper: Value) -> Value {
    let (Value::Object(lower), Value::Object(upper)) = (lower, upper) else {
        unreachable!("layers are JSON objects (see parse_object)");
    };
    let merged = upper.into_iter().fold(lower, |mut merged, (key, value)| {
        let value = match (key.as_str(), merged.remove(&key), value) {
            ("roles", Some(Value::Object(below)), Value::Object(above)) => {
                Value::Object(below.into_iter().chain(above).collect())
            }
            (_, _, value) => value,
        };
        merged.insert(key, value);
        merged
    });
    Value::Object(merged)
}

/// The effective configuration for the current working directory.
pub fn load_effective() -> anyhow::Result<Config> {
    ConfigFiles::locate(&std::env::current_dir()?).load()
}

/// Rewrites every role a configuration file names through `rewrite`, leaving
/// everything else in the file — and any role it does not name — untouched.
/// So a project file that only sets `verify` stays that small, and each file
/// keeps exactly the roles it had.
pub fn rewrite_roles(
    raw: &str,
    rewrite: impl Fn(&RoleConfig) -> RoleConfig,
) -> anyhow::Result<String> {
    // A role the rewrite leaves alone is kept as written, not re-serialized:
    // the file changes only where its meaning does.
    let rewrite_one = |role: Value| -> anyhow::Result<Value> {
        let parsed: RoleConfig = serde_json::from_value(role.clone())?;
        let rewritten = rewrite(&parsed);
        if rewritten == parsed {
            Ok(role)
        } else {
            Ok(serde_json::to_value(rewritten)?)
        }
    };
    let rewrite_named = |name: &str, role: Value| -> anyhow::Result<Value> {
        match (name, role) {
            ("workers", Value::Array(workers)) => Ok(Value::Array(
                workers
                    .into_iter()
                    .map(rewrite_one)
                    .collect::<anyhow::Result<_>>()?,
            )),
            (_, role) => rewrite_one(role),
        }
    };

    let Value::Object(file) = parse_object(raw)? else {
        unreachable!("parse_object returns a JSON object");
    };
    let rewritten = file
        .into_iter()
        .map(|(key, value)| match (key.as_str(), value) {
            ("roles", Value::Object(roles)) => {
                let roles = roles
                    .into_iter()
                    .map(|(name, role)| rewrite_named(&name, role).map(|role| (name, role)))
                    .collect::<anyhow::Result<serde_json::Map<_, _>>>()?;
                Ok((key, Value::Object(roles)))
            }
            (_, value) => Ok((key, value)),
        })
        .collect::<anyhow::Result<serde_json::Map<_, _>>>()?;
    Ok(serde_json::to_string_pretty(&Value::Object(rewritten))?)
}

/// Sets the given roles in a configuration file, leaving every other role and
/// key as it was.
pub fn set_roles(raw: &str, roles: serde_json::Map<String, Value>) -> anyhow::Result<String> {
    let Value::Object(mut file) = parse_object(raw)? else {
        unreachable!("parse_object returns a JSON object");
    };
    let existing = match file.remove("roles") {
        Some(Value::Object(existing)) => existing,
        _ => serde_json::Map::new(),
    };
    let merged: serde_json::Map<String, Value> = existing.into_iter().chain(roles).collect();
    file.insert("roles".to_string(), Value::Object(merged));
    Ok(serde_json::to_string_pretty(&Value::Object(file))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Provider;

    struct Dirs {
        _home: tempfile::TempDir,
        project: tempfile::TempDir,
        user_file: PathBuf,
    }

    fn dirs() -> Dirs {
        let home = tempfile::tempdir().unwrap();
        let user_file = home.path().join("consilium.config.json");
        Dirs {
            _home: home,
            project: tempfile::tempdir().unwrap(),
            user_file,
        }
    }

    impl Dirs {
        fn write_user(&self, json: &str) {
            std::fs::write(&self.user_file, json).unwrap();
        }
        fn write_project(&self, json: &str) {
            std::fs::write(self.project.path().join(PROJECT_CONFIG_FILE), json).unwrap();
        }
        fn files(&self) -> ConfigFiles {
            ConfigFiles::locate_with_user_file(self.project.path(), Some(self.user_file.clone()))
        }
        fn load(&self) -> anyhow::Result<Config> {
            self.files().load()
        }
    }

    /// A user's council: Fable conducts, two Antigravity workers.
    const USER_COUNCIL: &str = r#"{
      "roles": {
        "conductor":  { "provider": "claude", "model": "claude-fable-5-2" },
        "chairman":   { "provider": "claude", "model": "claude-fable-5-2" },
        "workers":    [
          { "provider": "gemini", "model": "gemini-4-argon-high" },
          { "provider": "gemini", "model": "gemini-4-hydrogen-high" }
        ],
        "reviewer":   { "provider": "codex", "model": "gpt-6-luna" },
        "supervisor": { "provider": "gemini", "model": "gemini-4-argon-high" }
      },
      "maxReplans": 2
    }"#;

    #[test]
    fn no_files_means_the_built_in_defaults() {
        let d = dirs();
        assert_eq!(d.files(), ConfigFiles::default());
        assert_eq!(d.load().unwrap(), Config::default());
    }

    /// The reason this module exists: the MCP server is started in whatever
    /// directory the session is in, and used to fall back to the built-in
    /// lineup everywhere but the one directory that held a config file.
    #[test]
    fn the_users_file_applies_in_a_project_without_one() {
        let d = dirs();
        d.write_user(USER_COUNCIL);
        let cfg = d.load().unwrap();
        assert_eq!(cfg.roles.conductor.model, "claude-fable-5-2");
        assert_eq!(cfg.roles.workers.len(), 2);
        assert_eq!(cfg.max_replans, 2);
    }

    #[test]
    fn a_project_file_that_only_sets_verify_keeps_the_users_council() {
        let d = dirs();
        d.write_user(USER_COUNCIL);
        d.write_project(r#"{ "verify": { "test": "cargo test" } }"#);
        let cfg = d.load().unwrap();
        assert_eq!(cfg.roles.conductor.model, "claude-fable-5-2");
        assert_eq!(cfg.verify.unwrap().test.as_deref(), Some("cargo test"));
        assert_eq!(cfg.max_replans, 2);
    }

    #[test]
    fn a_project_file_overrides_the_users_file_role_by_role() {
        let d = dirs();
        d.write_user(USER_COUNCIL);
        d.write_project(
            r#"{ "roles": { "reviewer": { "provider": "claude", "model": "claude-opus-5-5" } },
                 "maxReplans": 0 }"#,
        );
        let cfg = d.load().unwrap();
        assert_eq!(cfg.roles.reviewer.provider, Provider::Claude);
        assert_eq!(cfg.roles.reviewer.model, "claude-opus-5-5");
        // the roles the project does not name are still the user's
        assert_eq!(cfg.roles.conductor.model, "claude-fable-5-2");
        assert_eq!(cfg.roles.workers.len(), 2);
        assert_eq!(cfg.max_replans, 0);
    }

    #[test]
    fn a_project_file_alone_fills_what_it_leaves_out_from_the_defaults() {
        let d = dirs();
        d.write_project(r#"{ "verify": { "build": "make" } }"#);
        let cfg = d.load().unwrap();
        assert_eq!(cfg.roles, Config::default().roles);
        assert_eq!(cfg.verify.unwrap().build.as_deref(), Some("make"));
    }

    /// A complete project file read on its own is exactly what it says — the
    /// behaviour before layering existed.
    #[test]
    fn a_complete_project_file_reads_as_before() {
        let d = dirs();
        d.write_project(USER_COUNCIL);
        let direct: Config = serde_json::from_str(USER_COUNCIL).unwrap();
        assert_eq!(d.load().unwrap().roles, direct.roles);
    }

    #[test]
    fn an_explicit_null_clears_a_lower_layers_value() {
        let d = dirs();
        d.write_user(r#"{ "verify": { "test": "npm test" }, "budgetSecs": 600 }"#);
        d.write_project(r#"{ "verify": null, "budgetSecs": null }"#);
        let cfg = d.load().unwrap();
        assert!(cfg.verify.is_none());
        assert_eq!(cfg.budget_secs, None);
    }

    #[test]
    fn a_broken_file_is_an_error_that_names_it() {
        let d = dirs();
        d.write_user("{ not json");
        let err = format!("{:#}", d.load().unwrap_err());
        assert!(err.contains(&d.user_file.display().to_string()), "{err}");

        let d = dirs();
        d.write_project("[1, 2, 3]");
        let err = format!("{:#}", d.load().unwrap_err());
        assert!(err.contains(PROJECT_CONFIG_FILE), "{err}");
    }

    #[test]
    fn an_unknown_provider_is_still_rejected() {
        let d = dirs();
        d.write_project(r#"{ "roles": { "reviewer": { "provider": "warp", "model": "x" } } }"#);
        assert!(d.load().is_err());
    }

    #[test]
    fn existing_lists_the_users_file_before_the_projects() {
        let d = dirs();
        d.write_user(USER_COUNCIL);
        d.write_project("{}");
        let files = d.files();
        let project_file = d.project.path().join(PROJECT_CONFIG_FILE);
        assert_eq!(
            files.existing(),
            [d.user_file.as_path(), project_file.as_path()]
        );
        let described = files.describe().unwrap();
        assert!(described.contains(&d.user_file.display().to_string()));
        assert!(described.contains(PROJECT_CONFIG_FILE));
    }

    #[test]
    fn nothing_to_describe_without_files() {
        assert_eq!(dirs().files().describe(), None);
    }

    #[test]
    fn the_users_file_lives_in_the_consilium_home() {
        let path = user_config_path().expect("HOME is set in the test environment");
        assert!(
            path.ends_with(".consilium/consilium.config.json"),
            "{path:?}"
        );
    }

    fn renamed(role: &RoleConfig) -> RoleConfig {
        RoleConfig {
            model: format!("{}!", role.model),
            ..role.clone()
        }
    }

    #[test]
    fn rewrite_roles_touches_every_role_the_file_names() {
        let out = rewrite_roles(USER_COUNCIL, renamed).unwrap();
        let cfg: Config = serde_json::from_str(&out).unwrap();
        assert_eq!(cfg.roles.conductor.model, "claude-fable-5-2!");
        assert_eq!(cfg.roles.chairman.model, "claude-fable-5-2!");
        assert_eq!(cfg.roles.reviewer.model, "gpt-6-luna!");
        assert_eq!(cfg.roles.supervisor.model, "gemini-4-argon-high!");
        let workers: Vec<&str> = cfg.roles.workers.iter().map(|w| w.model.as_str()).collect();
        assert_eq!(workers, ["gemini-4-argon-high!", "gemini-4-hydrogen-high!"]);
        // everything else in the file is carried over
        assert_eq!(cfg.max_replans, 2);
    }

    /// A project file that sets only `verify` must not grow a `roles` block:
    /// that would pin a copy of the user's council in the project.
    #[test]
    fn rewrite_roles_leaves_a_partial_file_partial() {
        let raw = r#"{ "verify": { "test": "cargo test" },
                       "roles": { "reviewer": { "provider": "codex", "model": "gpt-5.5" } } }"#;
        let out: Value = serde_json::from_str(&rewrite_roles(raw, renamed).unwrap()).unwrap();
        assert_eq!(out["roles"]["reviewer"]["model"], "gpt-5.5!");
        assert_eq!(out["roles"].as_object().unwrap().len(), 1);
        assert_eq!(out["verify"]["test"], "cargo test");
        // and the file keeps its own key order, so a rewrite reads as an edit
        let keys: Vec<&String> = out.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["verify", "roles"]);

        let no_roles = r#"{ "verify": { "test": "cargo test" } }"#;
        let out: Value = serde_json::from_str(&rewrite_roles(no_roles, renamed).unwrap()).unwrap();
        assert!(out.get("roles").is_none());
    }

    /// A role the rewrite does not change stays as the user wrote it — no
    /// `"effort": null` and friends appear just because the file was read.
    #[test]
    fn rewrite_roles_leaves_an_unchanged_role_as_written() {
        let raw = r#"{ "roles": { "reviewer": { "provider": "codex", "model": "gpt-6.1-sol" } } }"#;
        let out: Value =
            serde_json::from_str(&rewrite_roles(raw, RoleConfig::clone).unwrap()).unwrap();
        assert_eq!(out, serde_json::from_str::<Value>(raw).unwrap());
    }

    #[test]
    fn set_roles_adds_roles_and_keeps_everything_else() {
        let reviewer = serde_json::json!({ "provider": "codex", "model": "gpt-6-luna" });
        let roles: serde_json::Map<String, Value> = [("reviewer".to_string(), reviewer.clone())]
            .into_iter()
            .collect();

        // a file with no roles yet gains a `roles` block
        let out: Value = serde_json::from_str(
            &set_roles(r#"{ "verify": { "test": "cargo test" } }"#, roles.clone()).unwrap(),
        )
        .unwrap();
        assert_eq!(out["roles"]["reviewer"], reviewer);
        assert_eq!(out["verify"]["test"], "cargo test");

        // a file that names other roles keeps them
        let raw = r#"{ "roles": { "conductor": { "provider": "claude", "model": "m" } } }"#;
        let out: Value = serde_json::from_str(&set_roles(raw, roles).unwrap()).unwrap();
        assert_eq!(out["roles"]["conductor"]["model"], "m");
        assert_eq!(out["roles"]["reviewer"], reviewer);
    }

    #[test]
    fn rewrite_roles_rejects_a_file_that_is_not_a_configuration() {
        assert!(rewrite_roles("{ not json", renamed).is_err());
        assert!(rewrite_roles("[]", renamed).is_err());
    }
}
