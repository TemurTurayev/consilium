//! Auth orchestrator: report each provider's auth state and the exact "detect +
//! guide" next step. Pure classifiers (this file's top half) are unit-tested;
//! the I/O shell (`probe_auth`/`auth_report`, added next) reuses `crate::doctor`
//! and is not unit-tested, mirroring `doctor::probe_model`.

use crate::adapters::CLI_DEFAULT_MODEL;
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

/// The CLI binary name for a provider. Covers every `Provider` variant, unlike
/// `doctor::run_doctor`'s fixed list (which deliberately excludes the beta,
/// opt-in Grok provider — see that function's doc comment).
pub fn cli_binary(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
        Provider::Gemini => "agy",
        Provider::Grok => "grok",
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
        Provider::Gemini => "run `agy` once and sign in with Google (it shares the Antigravity app's sign-in)",
        Provider::Grok => "run `grok login` and complete the browser sign-in (needs an X Premium+ / SuperGrok subscription)",
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
            format!(
                "{bin}: {} — retry, or run `{}` directly to see the error",
                doctor::one_line(detail, doctor::DETAIL_DISPLAY_CHARS),
                smoke_command(p)
            )
        }
    }
}

/// The shortest command that runs one prompt through a provider's CLI, for the
/// "try it yourself" hint. Each CLI spells its headless mode differently.
pub fn smoke_command(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "claude -p hi",
        Provider::Codex => "codex exec hi",
        Provider::Gemini => "agy -p hi",
        Provider::Grok => "grok -p hi",
    }
}

/// Probe one provider's auth state: CLI presence (`doctor::check`) then, if
/// present, a live liveness probe (`doctor::probe_model`, ~1 token) on the CLI's
/// OWN default model. Asking for no particular model keeps this a pure "is the
/// CLI installed and signed in?" check: it cannot be broken by a model the
/// provider has retired or one this account's plan does not include (both
/// happened to every v0.2.0 catalog model within four months). Which models
/// the account can run is a separate question — `crate::models`. I/O — not
/// unit-tested (spawns a real CLI).
pub async fn probe_auth(p: Provider, quota: &QuotaStore) -> ProviderAuth {
    let bin = cli_binary(p);
    if !doctor::check(bin).found {
        return ProviderAuth::CliMissing;
    }
    let adapter = doctor::adapter_for(p);
    let probe = doctor::probe_model(adapter, CLI_DEFAULT_MODEL, quota).await;
    classify(true, Some((probe.ok, &probe.detail)))
}

/// Probe all v1 providers concurrently, so a cold-starting Claude (~30s) does not
/// serialize the wait. Returns one (provider, status) per v1 provider, in a
/// stable order (claude, codex, gemini, grok).
pub async fn auth_report(quota: &QuotaStore) -> Vec<(Provider, ProviderAuth)> {
    let providers = [
        Provider::Claude,
        Provider::Codex,
        Provider::Gemini,
        Provider::Grok,
    ];
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
        assert_eq!(cli_binary(Provider::Grok), "grok");
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
        // agy 1.5 has no `login` subcommand (`agy help login` → "unknown
        // subcommand"): sign-in happens on its first interactive start.
        let g = guidance(Provider::Gemini, &ProviderAuth::NeedsLogin("401".into()));
        assert!(!g.contains("agy login"), "got: {g}");
        assert!(g.contains("run `agy`") && g.contains("sign in"), "got: {g}");
        let g = guidance(Provider::Grok, &ProviderAuth::NeedsLogin("401".into()));
        assert!(g.contains("`grok login`"), "got: {g}");
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
            !g.contains("sign in"),
            "Down must not suggest re-login: {g}"
        );
    }

    /// `codex -p` selects a config profile, not a prompt — the hint used to
    /// suggest `codex -p hi`, which does not run anything.
    #[test]
    fn guidance_for_down_names_a_command_that_runs_a_prompt() {
        let g = guidance(Provider::Codex, &ProviderAuth::Down("boom".into()));
        assert!(g.contains("`codex exec hi`"), "got: {g}");
        assert!(!g.contains("codex -p"), "got: {g}");
        let g = guidance(Provider::Gemini, &ProviderAuth::Down("boom".into()));
        assert!(g.contains("`agy -p hi`"), "got: {g}");
    }

    /// Real grok 0.2.87 refusal: a pretty-printed JSON blob spread over
    /// several lines. The status line must stay on one line.
    #[test]
    fn guidance_for_down_keeps_a_multi_line_error_on_one_line() {
        let detail = "transient failure: Internal error: {\n  \"message\": \"API error (status 426 Upgrade Required): Your Grok CLI version (0.2.87) is outdated.\"\n}";
        let g = guidance(Provider::Grok, &ProviderAuth::Down(detail.into()));
        assert!(!g.contains('\n'), "got: {g}");
        assert!(g.contains("is outdated"), "got: {g}");
    }

    #[test]
    fn smoke_command_starts_with_each_providers_binary() {
        for p in [
            Provider::Claude,
            Provider::Codex,
            Provider::Gemini,
            Provider::Grok,
        ] {
            assert!(
                smoke_command(p).starts_with(cli_binary(p)),
                "{}",
                smoke_command(p)
            );
        }
    }
}
