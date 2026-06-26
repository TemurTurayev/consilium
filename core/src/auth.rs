//! Auth orchestrator: report each provider's auth state and the exact "detect +
//! guide" next step. Pure classifiers (this file's top half) are unit-tested;
//! the I/O shell (`probe_auth`/`auth_report`, added next) reuses `crate::doctor`
//! and is not unit-tested, mirroring `doctor::probe_model`.

use crate::catalog::catalog;
use crate::doctor;
use crate::event::Provider;
use crate::quota::QuotaStore;

/// One provider's authentication state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAuth {
    /// The liveness probe succeeded — the provider answers.
    Ready,
    /// CLI is present but the probe failed in an auth-shaped way (carries the detail).
    NeedsLogin(String),
    /// The CLI binary is not on PATH.
    CliMissing,
    /// Present, but the probe failed for a non-auth reason (rate limit, transient…).
    Down(String),
}

/// The CLI binary name for a provider (matches `doctor::run_doctor`'s list).
pub fn cli_binary(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
        Provider::Gemini => "agy",
    }
}

/// True when a probe failure detail looks like an auth/credential problem (vs a
/// transient/other failure). Mirrors `doctor::remediation_hint`'s matching.
pub fn is_auth_failure(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    d.contains("401")
        || d.contains("403")
        || d.contains("authenticat")
        || d.contains("unauthor")
        || d.contains("forbidden")
        || d.contains("permission_denied")
        || d.contains("permission denied")
        || d.contains("token expired")
        || d.contains("session expired")
        || d.contains("expired")
        || d.contains("credential")
        || d.contains("setup-token")
        || d.contains("not logged in")
        || d.contains("please log in")
        || d.contains("login")
}

/// Classify a provider's auth state from (cli-present?, probe ok?, probe detail).
/// Pure: the caller does the presence check + probe and passes the booleans in.
/// `probe`: `None` = not probed (treated as Down); `Some((ok, detail))` = probed.
pub fn classify(found: bool, probe: Option<(bool, &str)>) -> ProviderAuth {
    if !found {
        return ProviderAuth::CliMissing;
    }
    match probe {
        Some((true, _)) => ProviderAuth::Ready,
        Some((false, detail)) if is_auth_failure(detail) => {
            ProviderAuth::NeedsLogin(detail.to_string())
        }
        Some((false, detail)) => ProviderAuth::Down(detail.to_string()),
        None => ProviderAuth::Down("not probed".to_string()),
    }
}

/// The login command to get a provider authenticated (the "guide" half).
pub fn login_command(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "run `claude setup-token`, then export CLAUDE_CODE_OAUTH_TOKEN=<token> (add it to your shell profile so it persists)",
        Provider::Codex => "run `codex login`",
        Provider::Gemini => "run `agy login`",
    }
}

/// A one-line, actionable guidance string for a provider's status.
pub fn guidance(p: Provider, status: &ProviderAuth) -> String {
    let bin = cli_binary(p);
    match status {
        ProviderAuth::Ready => format!("{bin}: ready"),
        ProviderAuth::CliMissing => {
            format!("{bin}: not installed — install the {bin} CLI and ensure it's on your PATH")
        }
        ProviderAuth::NeedsLogin(_) => format!("{bin}: {}", login_command(p)),
        ProviderAuth::Down(detail) => {
            format!("{bin}: {detail} — retry, or run `{bin} -p hi` directly to see the error")
        }
    }
}

/// The model probed to test a provider's auth — its first catalog entry (the
/// curated primary). Every v1 provider has at least one catalog entry.
pub fn primary_model(p: Provider) -> Option<String> {
    catalog()
        .into_iter()
        .find(|e| e.provider == p)
        .map(|e| e.model)
}

/// Probe one provider's auth state: CLI presence (`doctor::check`) then, if
/// present, a live liveness probe (`doctor::probe_model`, ~1 token) on its
/// primary catalog model. I/O — not unit-tested (spawns a real CLI).
pub async fn probe_auth(p: Provider, quota: &QuotaStore) -> ProviderAuth {
    let bin = cli_binary(p);
    if !doctor::check(bin).found {
        return ProviderAuth::CliMissing;
    }
    let Some(model) = primary_model(p) else {
        return ProviderAuth::Down(format!("no catalog model for {}", p.as_str()));
    };
    let adapter = doctor::adapter_for(p);
    let probe = doctor::probe_model(adapter, &model, quota).await;
    classify(true, Some((probe.ok, &probe.detail)))
}

/// Probe all v1 providers concurrently, so a cold-starting Claude (~30s) does not
/// serialize the wait. Returns one (provider, status) per v1 provider, in a
/// stable order (claude, codex, gemini).
pub async fn auth_report(quota: &QuotaStore) -> Vec<(Provider, ProviderAuth)> {
    let providers = [Provider::Claude, Provider::Codex, Provider::Gemini];
    let futs = providers
        .into_iter()
        .map(|p| async move { (p, probe_auth(p, quota).await) });
    futures::future::join_all(futs).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_binary_maps_each_provider() {
        assert_eq!(cli_binary(Provider::Claude), "claude");
        assert_eq!(cli_binary(Provider::Codex), "codex");
        assert_eq!(cli_binary(Provider::Gemini), "agy");
    }

    #[test]
    fn is_auth_failure_matches_auth_shaped_details() {
        assert!(is_auth_failure("API Error: 401 authentication_error"));
        assert!(is_auth_failure("invalid credentials"));
        assert!(is_auth_failure("Please log in to continue"));
        assert!(is_auth_failure("run claude setup-token"));
        // 403 / permission-denied / expired shapes (Codex org/tier, Gemini PERMISSION_DENIED)
        assert!(is_auth_failure("API Error: 403 Forbidden"));
        assert!(is_auth_failure("permission_denied"));
        assert!(is_auth_failure("Your token has expired"));
        assert!(is_auth_failure("session expired"));
        // non-auth failures must not be mis-classified
        assert!(!is_auth_failure("rate limit exceeded"));
        assert!(!is_auth_failure("connection timed out"));
    }

    #[test]
    fn classify_missing_cli() {
        assert_eq!(classify(false, None), ProviderAuth::CliMissing);
        assert_eq!(
            classify(false, Some((true, "ok"))),
            ProviderAuth::CliMissing
        );
    }

    #[test]
    fn classify_ready_needs_login_and_down() {
        assert_eq!(classify(true, Some((true, "ok"))), ProviderAuth::Ready);
        assert_eq!(
            classify(true, Some((false, "401 unauthorized"))),
            ProviderAuth::NeedsLogin("401 unauthorized".to_string())
        );
        assert_eq!(
            classify(true, Some((false, "rate limit exceeded"))),
            ProviderAuth::Down("rate limit exceeded".to_string())
        );
        assert_eq!(
            classify(true, None),
            ProviderAuth::Down("not probed".to_string())
        );
    }

    #[test]
    fn guidance_gives_login_command_for_needs_login() {
        let g = guidance(Provider::Claude, &ProviderAuth::NeedsLogin("401".into()));
        assert!(g.contains("setup-token"), "got: {g}");
        let g = guidance(Provider::Codex, &ProviderAuth::NeedsLogin("401".into()));
        assert!(g.contains("codex login"), "got: {g}");
        let g = guidance(Provider::Gemini, &ProviderAuth::NeedsLogin("401".into()));
        assert!(g.contains("agy login"), "got: {g}");
    }

    #[test]
    fn guidance_for_missing_says_install() {
        let g = guidance(Provider::Codex, &ProviderAuth::CliMissing);
        assert!(
            g.contains("not installed") && g.contains("PATH"),
            "got: {g}"
        );
    }

    #[test]
    fn guidance_for_down_echoes_detail_not_login() {
        let g = guidance(Provider::Gemini, &ProviderAuth::Down("rate limited".into()));
        assert!(g.contains("rate limited"), "got: {g}");
        assert!(
            !g.contains("agy login"),
            "Down must not suggest re-login: {g}"
        );
    }

    #[test]
    fn primary_model_is_the_first_catalog_entry_per_provider() {
        assert_eq!(
            primary_model(Provider::Claude).as_deref(),
            Some("claude-opus-4-8")
        );
        assert_eq!(primary_model(Provider::Codex).as_deref(), Some("gpt-5.5"));
        assert!(primary_model(Provider::Gemini).is_some());
    }
}
